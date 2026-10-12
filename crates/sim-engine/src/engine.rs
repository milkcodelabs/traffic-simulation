use std::collections::VecDeque;
use std::sync::Arc;

use rayon::prelude::*;
use sim_core::{
    scaled_capacity, storage_inflow_ceiling, EdgeId, LinkTransmissionState, Network, NodeId, Scenario,
    SignalController, SignalMode, SignalSchedule, TAU_YIELD_S, TimeWindow,
};
use thiserror::Error;

use crate::route::{experienced_weights, free_flow_weights, path_cost, shortest_path, should_reroute};

/// One trip released by the caller. This is not a synthetic origin-destination matrix.
#[derive(Clone, Debug)]
pub struct Trip {
    pub depart_s: f64,
    pub origin: NodeId,
    pub destination: NodeId,
    /// `None` uses the scenario inertia. A value outside the usual 1.3–1.5 band is allowed
    /// so a test can pin a driver to a habitual path.
    pub inertia: Option<f64>,
}

impl Trip {
    pub fn new(depart_s: f64, origin: NodeId, destination: NodeId) -> Self {
        Self {
            depart_s,
            origin,
            destination,
            inertia: None,
        }
    }

    pub fn with_inertia(mut self, inertia: f64) -> Self {
        self.inertia = Some(inertia);
        self
    }
}

#[derive(Clone, Debug)]
pub struct Vehicle {
    pub id: u64,
    pub origin: NodeId,
    pub destination: NodeId,
    pub depart_s: f64,
    pub arrive_s: Option<f64>,
    /// Edges from the origin through the last link into the destination.
    pub path: Vec<EdgeId>,
    /// Index of the occupied edge while `on_link` is set. While waiting, the next entry is `path[0]`.
    pub cursor: usize,
    pub on_link: bool,
    pub enter_s: f64,
    /// Free-flow time of the path chosen at departure. Delay is measured against this, not against a later reroute.
    pub habitual_s: f64,
    /// Free-flow time of the path the driver currently expects, from the last decision node.
    pub baseline_s: f64,
    pub inertia: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Census {
    pub spawned: u64,
    pub active: u64,
    pub completed: u64,
    pub on_links: u64,
    pub waiting: u64,
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("time step must be positive")]
    BadStep,
    #[error("conservation failed: spawned {spawned} != active {active} + completed {completed}")]
    Conservation { spawned: u64, active: u64, completed: u64 },
    #[error("edge {edge} queue holds {queued} vehicles but the cumulative counts say {counted}")]
    LinkMismatch { edge: u32, queued: usize, counted: f64 },
    #[error("edge {edge} holds {queued} vehicles above its storage {storage}")]
    Storage { edge: u32, queued: usize, storage: f64 },
}

#[derive(Clone)]
pub(crate) struct LinkQueues {
    queues: Vec<VecDeque<u64>>,
}

impl LinkQueues {
    fn new(edge_count: usize) -> Self {
        Self {
            queues: (0..edge_count).map(|_| VecDeque::new()).collect(),
        }
    }

    pub(crate) fn vehicles(&self, edge: EdgeId) -> usize {
        self.queues[edge.index()].len()
    }
}

struct MoveOut {
    vehicle: u64,
    edge: EdgeId,
    at: f64,
    kind: MoveKind,
}

enum MoveKind {
    Transfer {
        to: EdgeId,
        path: Vec<EdgeId>,
        cursor: usize,
        baseline_s: f64,
    },
    Complete,
}

struct Entry {
    vehicle: u64,
    edge: EdgeId,
    at: f64,
    path: Vec<EdgeId>,
    baseline_s: f64,
    habitual_s: f64,
}

struct NodeProposal {
    departures: Vec<MoveOut>,
    entries: Vec<Entry>,
    credits: Vec<(usize, f64)>,
    demand: Vec<(usize, u32)>,
}

/// Mutable stepper state, without the shared graph. A desktop session keeps these so a
/// scrub can resume from a recorded instant instead of cloning the road network.
#[derive(Clone)]
pub struct SimSnapshot {
    pub scenario: Scenario,
    pub clock_origin_s: f64,
    pub time_s: f64,
    pub links: LinkTransmissionState,
    pub queues: Vec<VecDeque<u64>>,
    pub discharge_credit: Vec<f64>,
    pub free_flow: Vec<f64>,
    pub vehicles: Vec<Vehicle>,
    pub waiting: Vec<Vec<u64>>,
    pub pending: Vec<Trip>,
    pub spawned: u64,
    pub completed: u64,
    pub vht_s: f64,
    pub vkt_m: f64,
    pub delay_s: f64,
}

/// Mesoscopic link-transmission / spatial-queue stepper.
pub struct Simulation {
    network: Arc<Network>,
    scenario: Scenario,
    /// Seconds from midnight corresponding to `time_s == 0`.
    clock_origin_s: f64,
    time_s: f64,
    links: LinkTransmissionState,
    queues: LinkQueues,
    /// Fractional part of `q Δt` still owed to each link. Always in `[0, 1)` after an open step.
    discharge_credit: Vec<f64>,
    free_flow: Vec<f64>,
    vehicles: Vec<Vehicle>,
    waiting: Vec<Vec<u64>>,
    pending: Vec<Trip>,
    spawned: u64,
    completed: u64,
    /// Vehicle-hours of everyone active during each step, in seconds.
    vht_s: f64,
    /// Metres travelled by vehicles that have left a link.
    vkt_m: f64,
    /// Extra seconds spent on a link beyond its free-flow time, summed at departure.
    delay_s: f64,
    /// Bumps when a vehicle enters or leaves a link, which is when the drawn speeds can change.
    appearance: u64,
}

impl Simulation {
    pub fn new(network: Network, scenario: Scenario) -> Self {
        Self::fresh(Arc::new(network), scenario)
    }

    pub fn shared(network: Arc<Network>, scenario: Scenario) -> Self {
        Self::fresh(network, scenario)
    }

    fn fresh(network: Arc<Network>, scenario: Scenario) -> Self {
        let edges = network.edges.len();
        let nodes = network.nodes.len();
        let free_flow = free_flow_weights(&network, &scenario);
        Self {
            network,
            scenario,
            clock_origin_s: 12.0 * 3600.0,
            time_s: 0.0,
            links: LinkTransmissionState::empty(edges),
            queues: LinkQueues::new(edges),
            discharge_credit: vec![0.0; edges],
            free_flow,
            vehicles: Vec::new(),
            waiting: vec![Vec::new(); nodes],
            pending: Vec::new(),
            spawned: 0,
            completed: 0,
            vht_s: 0.0,
            vkt_m: 0.0,
            delay_s: 0.0,
            appearance: 0,
        }
    }

    pub fn restore(network: Arc<Network>, snap: SimSnapshot) -> Self {
        Self {
            network,
            scenario: snap.scenario,
            clock_origin_s: snap.clock_origin_s,
            time_s: snap.time_s,
            links: snap.links,
            queues: LinkQueues { queues: snap.queues },
            discharge_credit: snap.discharge_credit,
            free_flow: snap.free_flow,
            vehicles: snap.vehicles,
            waiting: snap.waiting,
            pending: snap.pending,
            spawned: snap.spawned,
            completed: snap.completed,
            vht_s: snap.vht_s,
            vkt_m: snap.vkt_m,
            delay_s: snap.delay_s,
            appearance: 0,
        }
    }

    /// Changes only when link contents change. The desktop thread uses it to avoid
    /// rebuilding an identical congestion picture.
    pub fn appearance(&self) -> u64 {
        self.appearance
    }

    pub fn snapshot(&self) -> SimSnapshot {
        SimSnapshot {
            scenario: self.scenario.clone(),
            clock_origin_s: self.clock_origin_s,
            time_s: self.time_s,
            links: self.links.clone(),
            queues: self.queues.queues.clone(),
            discharge_credit: self.discharge_credit.clone(),
            free_flow: self.free_flow.clone(),
            vehicles: self.vehicles.clone(),
            waiting: self.waiting.clone(),
            pending: self.pending.clone(),
            spawned: self.spawned,
            completed: self.completed,
            vht_s: self.vht_s,
            vkt_m: self.vkt_m,
            delay_s: self.delay_s,
        }
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    pub fn scenario_mut(&mut self) -> &mut Scenario {
        &mut self.scenario
    }

    pub fn time(&self) -> f64 {
        self.time_s
    }

    /// `0` is midnight. The desktop shell uses this so the scrubber and the signal clock agree.
    /// Tests keep the noon default from [`Simulation::new`].
    pub fn set_clock_origin(&mut self, seconds_from_midnight: f64) {
        self.clock_origin_s = seconds_from_midnight.rem_euclid(86_400.0);
    }

    pub fn vht_s(&self) -> f64 {
        self.vht_s
    }

    pub fn vkt_m(&self) -> f64 {
        self.vkt_m
    }

    pub fn delay_s(&self) -> f64 {
        self.delay_s
    }

    /// Length-weighted instantaneous speed. An empty link contributes its free-flow speed.
    pub fn mean_speed_mps(&self) -> f64 {
        let mut weighted = 0.0;
        let mut length = 0.0;
        for index in 0..self.network.edges.len() {
            let edge = &self.network.edges[index];
            let travel = self.free_flow[index];
            if edge.length_m <= 0.0 || !travel.is_finite() || travel <= 1e-12 {
                continue;
            }
            let (ratio, _, _) = self.link_observation(index);
            weighted += (edge.length_m / travel) * ratio * edge.length_m;
            length += edge.length_m;
        }
        if length <= 0.0 {
            0.0
        } else {
            weighted / length
        }
    }

    /// Per-edge drawing fields. `speed_ratio` is `v / v_free` in `[0, 1]`. `density_ratio` is
    /// `N / S_max`. `queue_veh` counts vehicles that have already spent their free-flow time
    /// and are waiting to discharge.
    pub fn write_link_fields(&self, speed_ratio: &mut [f32], density_ratio: &mut [f32], queue_veh: &mut [f32]) {
        let count = self
            .network
            .edges
            .len()
            .min(speed_ratio.len())
            .min(density_ratio.len())
            .min(queue_veh.len());
        for index in 0..count {
            let (ratio, density, queued) = self.link_observation(index);
            speed_ratio[index] = ratio as f32;
            density_ratio[index] = density as f32;
            queue_veh[index] = queued as f32;
        }
    }

    fn link_observation(&self, index: usize) -> (f64, f64, f64) {
        let edge = &self.network.edges[index];
        let count = self.queues.queues[index].len();
        let travel = self.free_flow[index];
        let capacity = scaled_capacity(edge.capacity_veh_per_s, EdgeId(index as u32), &self.scenario);
        let ratio = if count == 0 {
            1.0
        } else if !travel.is_finite() || travel <= 1e-12 {
            0.0
        } else {
            let experienced = if capacity <= 1e-12 {
                f64::INFINITY
            } else {
                travel.max(count as f64 / capacity)
            };
            if experienced.is_finite() && experienced > 0.0 {
                (travel / experienced).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let density = if edge.storage_max_veh > 1e-9 {
            (count as f64 / edge.storage_max_veh).clamp(0.0, 1.0)
        } else if count == 0 {
            0.0
        } else {
            1.0
        };
        let queued = if travel.is_finite() {
            self.queues.queues[index]
                .iter()
                .filter(|id| {
                    self.vehicles
                        .get(**id as usize)
                        .is_some_and(|vehicle| vehicle.enter_s + travel <= self.time_s + 1e-9)
                })
                .count()
        } else {
            count
        };
        (ratio, density, queued as f64)
    }

    pub fn link_vehicles(&self, edge: EdgeId) -> usize {
        self.queues.vehicles(edge)
    }

    pub fn vehicle(&self, id: u64) -> Option<&Vehicle> {
        self.vehicles.get(id as usize)
    }

    pub fn vehicles(&self) -> &[Vehicle] {
        &self.vehicles
    }

    pub fn census(&self) -> Census {
        let on_links = self.queues.queues.iter().map(|queue| queue.len() as u64).sum();
        let waiting = self.waiting.iter().map(|queue| queue.len() as u64).sum();
        Census {
            spawned: self.spawned,
            active: on_links + waiting,
            completed: self.completed,
            on_links,
            waiting,
        }
    }

    pub fn schedule(&mut self, trips: impl IntoIterator<Item = Trip>) {
        self.pending.extend(trips);
        self.pending.sort_by(|left, right| left.depart_s.total_cmp(&right.depart_s));
    }

    pub fn advance(&mut self, horizon_s: f64, dt: f64) -> Result<(), EngineError> {
        if dt <= 0.0 {
            return Err(EngineError::BadStep);
        }
        while self.time_s + 1e-9 < horizon_s {
            let step = dt.min(horizon_s - self.time_s);
            self.step(step)?;
        }
        Ok(())
    }

    pub fn step(&mut self, dt: f64) -> Result<(), EngineError> {
        if dt <= 0.0 {
            return Err(EngineError::BadStep);
        }
        let t1 = self.time_s + dt;
        let trip_due = self.pending.first().is_some_and(|trip| trip.depart_s <= t1);
        if !trip_due && self.spawned == self.completed {
            self.tick_idle_credits(dt);
            self.time_s = t1;
            return Ok(());
        }
        self.release(t1);
        let active_during = self.census().active as f64;
        let experienced = experienced_weights(&self.network, &self.scenario, &self.queues);
        let rooms = self.receiving_rooms();
        let node_count = self.network.nodes.len();
        let proposals: Vec<NodeProposal> = (0..node_count)
            .into_par_iter()
            .map(|node| self.propose(node, dt, t1, &experienced, &rooms))
            .collect();
        self.apply(&proposals, t1);
        self.time_s = t1;
        self.vht_s += active_during * dt;
        self.audit()
    }

    fn release(&mut self, t1: f64) {
        let split = self
            .pending
            .iter()
            .position(|trip| trip.depart_s > t1)
            .unwrap_or(self.pending.len());
        let due: Vec<Trip> = self.pending.drain(..split).collect();
        for trip in due {
            let id = self.vehicles.len() as u64;
            let inertia = trip.inertia.unwrap_or(self.scenario.route_inertia);
            let routed = shortest_path(&self.network, &self.free_flow, trip.origin, trip.destination);
            let (path, habitual) = routed.unwrap_or((Vec::new(), f64::INFINITY));
            self.vehicles.push(Vehicle {
                id,
                origin: trip.origin,
                destination: trip.destination,
                depart_s: trip.depart_s,
                arrive_s: None,
                path,
                cursor: 0,
                on_link: false,
                enter_s: trip.depart_s,
                habitual_s: habitual,
                baseline_s: habitual,
                inertia,
            });
            self.spawned += 1;
            if trip.origin == trip.destination {
                self.vehicles[id as usize].arrive_s = Some(trip.depart_s);
                self.completed += 1;
            } else {
                self.waiting[trip.origin.index()].push(id);
            }
        }
    }

    fn propose(&self, node_index: usize, dt: f64, t1: f64, experienced: &[f64], rooms: &[u32]) -> NodeProposal {
        let node = NodeId(node_index as u32);
        let mut proposal = NodeProposal {
            departures: Vec::new(),
            entries: Vec::new(),
            credits: Vec::new(),
            demand: Vec::new(),
        };
        let major_conflict = self.priority_conflict(node, t1);
        let mut incoming: Vec<EdgeId> = self.network.adjacency.incoming(node).to_vec();
        incoming.sort_by_key(|edge| edge.0);
        let busy = !self.waiting[node.index()].is_empty()
            || incoming.iter().any(|edge| !self.queues.queues[edge.index()].is_empty());
        let mut room = if busy { rooms.to_vec() } else { Vec::new() };
        for edge in incoming {
            let open = self.approach_open(edge, major_conflict);
            let capacity = scaled_capacity(self.network.edges[edge.index()].capacity_veh_per_s, edge, &self.scenario);
            let (slots, credit) = discharge_slots(open, capacity, self.discharge_credit[edge.index()], dt);
            proposal.credits.push((edge.index(), credit));
            self.drain_edge(node, edge, slots, t1, experienced, &mut room, &mut proposal);
        }
        self.drain_origin(node, t1, experienced, &mut room, &mut proposal);
        proposal
    }

    fn drain_edge(
        &self,
        node: NodeId,
        edge: EdgeId,
        mut slots: u32,
        t1: f64,
        experienced: &[f64],
        room: &mut [u32],
        proposal: &mut NodeProposal,
    ) {
        for (position, vehicle_id) in self.queues.queues[edge.index()].iter().enumerate() {
            if slots == 0 {
                break;
            }
            let vehicle = &self.vehicles[*vehicle_id as usize];
            let ready = self.ready_time(vehicle, edge, node);
            if ready > t1 {
                break;
            }
            if node == vehicle.destination {
                proposal.departures.push(MoveOut {
                    vehicle: vehicle.id,
                    edge,
                    at: ready.max(self.time_s),
                    kind: MoveKind::Complete,
                });
                slots -= 1;
                continue;
            }
            let decision = self.decide_next(vehicle, node, experienced);
            let Some(next) = decision.next else {
                break;
            };
            if !self.turn_ok(node, edge, next) || room[next.index()] == 0 {
                if room[next.index()] == 0 {
                    let held = self.queues.queues[edge.index()]
                        .iter()
                        .skip(position)
                        .filter(|id| self.ready_time(&self.vehicles[**id as usize], edge, node) <= t1)
                        .count() as u32;
                    proposal.demand.push((next.index(), held));
                }
                break;
            }
            room[next.index()] -= 1;
            proposal.departures.push(MoveOut {
                vehicle: vehicle.id,
                edge,
                at: ready.max(self.time_s),
                kind: MoveKind::Transfer {
                    to: next,
                    path: decision.path,
                    cursor: decision.cursor,
                    baseline_s: decision.baseline_s,
                },
            });
            slots -= 1;
        }
    }

    fn drain_origin(
        &self,
        node: NodeId,
        t1: f64,
        experienced: &[f64],
        room: &mut [u32],
        proposal: &mut NodeProposal,
    ) {
        let waiting = &self.waiting[node.index()];
        if waiting.is_empty() {
            return;
        }
        let mut plans = Vec::with_capacity(waiting.len());
        for id in waiting {
            let vehicle = &self.vehicles[*id as usize];
            if vehicle.depart_s > t1 {
                plans.push(None);
                continue;
            }
            plans.push(Some(self.decide_entry(vehicle, node, experienced)));
        }
        let mut entered = vec![false; waiting.len()];
        let outgoing = self.network.adjacency.outgoing(node);
        for edge in outgoing {
            let mut left = room[edge.index()];
            if left == 0 {
                continue;
            }
            for (index, plan) in plans.iter().enumerate() {
                let Some(plan) = plan else { continue };
                if plan.edge != *edge || entered[index] {
                    continue;
                }
                if left == 0 {
                    break;
                }
                left -= 1;
                entered[index] = true;
                let vehicle = &self.vehicles[waiting[index] as usize];
                proposal.entries.push(Entry {
                    vehicle: vehicle.id,
                    edge: *edge,
                    at: vehicle.depart_s.max(self.time_s),
                    path: plan.path.clone(),
                    baseline_s: plan.baseline_s,
                    habitual_s: plan.habitual_s,
                });
            }
            room[edge.index()] = left;
        }
    }

    fn decide_entry(&self, vehicle: &Vehicle, node: NodeId, experienced: &[f64]) -> EntryPlan {
        let mut path = vehicle.path.clone();
        let mut baseline = vehicle.baseline_s;
        let mut habitual = vehicle.habitual_s;
        if path.is_empty() || self.network.edges[path[0].index()].from != node {
            if let Some((replacement, cost)) = shortest_path(&self.network, &self.free_flow, node, vehicle.destination) {
                path = replacement;
                baseline = cost;
                if !habitual.is_finite() {
                    habitual = cost;
                }
            }
        }
        let experienced_cost = path_cost(&path, experienced);
        let free_cost = path_cost(&path, &self.free_flow);
        if should_reroute(experienced_cost, free_cost, vehicle.inertia) {
            if let Some((replacement, _)) = shortest_path(&self.network, experienced, node, vehicle.destination) {
                if replacement.first() != path.first() {
                    baseline = path_cost(&replacement, &self.free_flow);
                    path = replacement;
                }
            }
        }
        let edge = path.first().copied().unwrap_or(EdgeId::INVALID);
        EntryPlan {
            edge,
            path,
            baseline_s: baseline,
            habitual_s: habitual,
        }
    }

    fn decide_next(&self, vehicle: &Vehicle, node: NodeId, experienced: &[f64]) -> NextPlan {
        let suffix = if vehicle.cursor + 1 <= vehicle.path.len() {
            vehicle.path[vehicle.cursor + 1..].to_vec()
        } else {
            Vec::new()
        };
        let mut path = vehicle.path.clone();
        let mut baseline = path_cost(&suffix, &self.free_flow);
        let experienced_cost = path_cost(&suffix, experienced);
        if should_reroute(experienced_cost, baseline, vehicle.inertia) {
            if let Some((replacement, _)) = shortest_path(&self.network, experienced, node, vehicle.destination) {
                let old_next = suffix.first().copied();
                if replacement.first().copied() != old_next {
                    path.truncate(vehicle.cursor + 1);
                    path.extend(replacement.iter().copied());
                    baseline = path_cost(&replacement, &self.free_flow);
                }
            }
        }
        let next = path.get(vehicle.cursor + 1).copied();
        let cursor = vehicle.cursor + 1;
        NextPlan {
            next,
            path,
            cursor,
            baseline_s: baseline,
        }
    }

    fn apply(&mut self, proposals: &[NodeProposal], t1: f64) {
        let _ = t1;
        let mut demand = vec![0u32; self.network.edges.len()];
        for proposal in proposals {
            for (edge, credit) in &proposal.credits {
                self.discharge_credit[*edge] = *credit;
            }
            for (edge, count) in &proposal.demand {
                demand[*edge] = demand[*edge].saturating_add(*count);
            }
        }
        for proposal in proposals {
            for departure in &proposal.departures {
                let queue = &mut self.queues.queues[departure.edge.index()];
                let front = queue.pop_front().expect("departure matches the FIFO prefix");
                assert_eq!(front, departure.vehicle);
                self.links.cumulative_out[departure.edge.index()] += 1.0;
                let edge_index = departure.edge.index();
                let spent = (departure.at - self.vehicles[departure.vehicle as usize].enter_s).max(0.0);
                let travel = self.free_flow[edge_index];
                if travel.is_finite() {
                    self.delay_s += (spent - travel).max(0.0);
                }
                self.vkt_m += self.network.edges[edge_index].length_m;
                let vehicle = &mut self.vehicles[departure.vehicle as usize];
                match &departure.kind {
                    MoveKind::Complete => {
                        vehicle.on_link = false;
                        vehicle.arrive_s = Some(departure.at);
                        self.completed += 1;
                    }
                    MoveKind::Transfer { path, cursor, baseline_s, .. } => {
                        vehicle.path.clone_from(path);
                        vehicle.cursor = *cursor;
                        vehicle.baseline_s = *baseline_s;
                        vehicle.on_link = false;
                    }
                }
            }
        }
        for proposal in proposals {
            for entry in &proposal.entries {
                self.waiting[self.network.edges[entry.edge.index()].from.index()]
                    .retain(|id| *id != entry.vehicle);
                let vehicle = &mut self.vehicles[entry.vehicle as usize];
                vehicle.path.clone_from(&entry.path);
                vehicle.cursor = 0;
                vehicle.on_link = true;
                vehicle.enter_s = entry.at;
                vehicle.baseline_s = entry.baseline_s;
                if entry.habitual_s.is_finite() {
                    vehicle.habitual_s = entry.habitual_s;
                }
                self.queues.queues[entry.edge.index()].push_back(entry.vehicle);
                self.links.cumulative_in[entry.edge.index()] += 1.0;
            }
        }
        for proposal in proposals {
            for departure in &proposal.departures {
                let MoveKind::Transfer {
                    to,
                    path: _,
                    cursor: _,
                    baseline_s: _,
                } = &departure.kind
                else {
                    continue;
                };
                let vehicle = &mut self.vehicles[departure.vehicle as usize];
                vehicle.on_link = true;
                vehicle.enter_s = departure.at;
                self.queues.queues[to.index()].push_back(departure.vehicle);
                self.links.cumulative_in[to.index()] += 1.0;
            }
        }
        for (edge, count) in demand.iter().enumerate() {
            self.links.cumulative_demand[edge] = f64::from(*count);
        }
        if proposals.iter().any(|proposal| !proposal.departures.is_empty() || !proposal.entries.is_empty()) {
            self.appearance = self.appearance.wrapping_add(1);
        }
    }

    fn audit(&self) -> Result<(), EngineError> {
        let census = self.census();
        if census.spawned != census.active + census.completed {
            return Err(EngineError::Conservation {
                spawned: census.spawned,
                active: census.active,
                completed: census.completed,
            });
        }
        for (index, queue) in self.queues.queues.iter().enumerate() {
            let counted = self.links.vehicles(EdgeId(index as u32));
            if (counted - queue.len() as f64).abs() > 1e-6 {
                return Err(EngineError::LinkMismatch {
                    edge: index as u32,
                    queued: queue.len(),
                    counted,
                });
            }
            let storage = self.network.edges[index].storage_max_veh;
            if queue.len() as f64 > storage + 1e-6 {
                return Err(EngineError::Storage {
                    edge: index as u32,
                    queued: queue.len(),
                    storage,
                });
            }
        }
        Ok(())
    }

    /// Moves an empty network forward by whole seconds. Returns 0 when a vehicle or a due trip
    /// is present, because those still need the normal step.
    pub fn idle_advance(&mut self, seconds: u32) -> u32 {
        if seconds == 0 || self.spawned != self.completed {
            return 0;
        }
        let horizon = self.time_s + f64::from(seconds);
        if self.pending.first().is_some_and(|trip| trip.depart_s <= horizon + 1e-9) {
            return 0;
        }
        let mut left = seconds;
        while left > 0 {
            let chunk = self.stable_idle_seconds(left);
            self.accrue_idle_seconds(chunk);
            self.time_s += f64::from(chunk);
            left -= chunk;
        }
        seconds
    }

    /// Same discharge remainder an empty network would store, without proposing moves.
    fn tick_idle_credits(&mut self, dt: f64) {
        for index in 0..self.network.edges.len() {
            let edge = EdgeId(index as u32);
            let capacity = scaled_capacity(self.network.edges[index].capacity_veh_per_s, edge, &self.scenario);
            let open = self.approach_open(edge, false);
            let (_, credit) = discharge_slots(open, capacity, self.discharge_credit[index], dt);
            self.discharge_credit[index] = credit;
        }
    }

    fn stable_idle_seconds(&self, max_seconds: u32) -> u32 {
        if max_seconds == 0 {
            return 0;
        }
        if !matches!(self.scenario.signal_schedule, SignalSchedule::Unchanged) {
            return max_seconds;
        }
        let tod = self.time_of_day();
        let mut limit = max_seconds;
        for signal in &self.network.signals {
            limit = limit.min(seconds_until_window_boundary(tod, signal.night_window));
        }
        limit.max(1)
    }

    fn accrue_idle_seconds(&mut self, steps: u32) {
        if steps == 0 {
            return;
        }
        let priority = self.priority_nodes();
        let bare = self.scenario.edge_capacity_scale.is_empty();
        let mut by_signal: Vec<Vec<usize>> = vec![Vec::new(); self.network.signals.len()];
        for index in 0..self.network.edges.len() {
            let head = self.network.edges[index].to.index();
            if priority[head] {
                self.add_open_credit(index, steps, bare);
            } else if let Some(signal) = self.network.nodes[head].signal {
                by_signal[signal.index()].push(index);
            }
        }
        for (signal_index, edges) in by_signal.into_iter().enumerate() {
            if !edges.is_empty() {
                self.accrue_fixed_signal(signal_index, &edges, steps, bare);
            }
        }
    }

    fn accrue_fixed_signal(&mut self, signal_index: usize, edges: &[usize], steps: u32, bare: bool) {
        let signal = &self.network.signals[signal_index];
        let cycle = signal.cycle_length_s;
        if cycle <= 0.0 || signal.phases.is_empty() {
            return;
        }
        let base = (self.time_s + self.clock_origin_s + signal.offset_s).rem_euclid(cycle);
        let intervals: Vec<(usize, Vec<(f64, f64)>)> = edges
            .iter()
            .copied()
            .map(|index| (index, open_intervals(signal, EdgeId(index as u32))))
            .collect();
        let mut period = 0u32;
        let search = steps.min(16_384);
        for candidate in 1..=search {
            let turns = f64::from(candidate) / cycle;
            if (turns - turns.round()).abs() > 1e-8 {
                continue;
            }
            let local = (base + f64::from(candidate)).rem_euclid(cycle);
            if (local - base).abs() < 1e-5 {
                period = candidate;
                break;
            }
        }
        if period > 0 {
            for (index, intervals) in &intervals {
                let mut pattern = 0u32;
                for step in 0..period {
                    let local = (base + f64::from(step)).rem_euclid(cycle);
                    if local_is_open(local, intervals) {
                        pattern += 1;
                    }
                }
                let full = steps / period;
                let remainder = steps % period;
                let mut open = full.saturating_mul(pattern);
                let skip = full * period;
                for step in 0..remainder {
                    let local = (base + f64::from(skip + step)).rem_euclid(cycle);
                    if local_is_open(local, intervals) {
                        open += 1;
                    }
                }
                self.add_open_credit(*index, open, bare);
            }
            return;
        }
        let mut local = base;
        let mut opens = vec![0u32; intervals.len()];
        for _ in 0..steps {
            for (slot, (_, edge_intervals)) in intervals.iter().enumerate() {
                if local_is_open(local, edge_intervals) {
                    opens[slot] += 1;
                }
            }
            local = (local + 1.0).rem_euclid(cycle);
        }
        for (slot, (index, _)) in intervals.iter().enumerate() {
            self.add_open_credit(*index, opens[slot], bare);
        }
    }

    fn add_open_credit(&mut self, index: usize, open_steps: u32, bare: bool) {
        if open_steps == 0 {
            return;
        }
        let capacity = self.edge_capacity(index, bare);
        if capacity <= 1e-12 {
            return;
        }
        let offered = self.discharge_credit[index] + capacity * f64::from(open_steps);
        self.discharge_credit[index] = offered - offered.floor();
    }

    fn edge_capacity(&self, index: usize, bare: bool) -> f64 {
        let base = self.network.edges[index].capacity_veh_per_s;
        if bare {
            base.max(0.0)
        } else {
            scaled_capacity(base, EdgeId(index as u32), &self.scenario)
        }
    }

    fn priority_nodes(&self) -> Vec<bool> {
        self.network
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                node.signal.is_none() || matches!(self.node_mode(NodeId(index as u32)), NodeControl::Priority)
            })
            .collect()
    }

    fn receiving_rooms(&self) -> Vec<u32> {
        self.network
            .edges
            .iter()
            .enumerate()
            .map(|(index, edge)| {
                let id = EdgeId(index as u32);
                let capacity = scaled_capacity(edge.capacity_veh_per_s, id, &self.scenario);
                if capacity <= 1e-12 {
                    return 0;
                }
                let occupied = self.queues.vehicles(id) as f64;
                storage_inflow_ceiling(edge.storage_max_veh, occupied).floor() as u32
            })
            .collect()
    }

    fn approach_open(&self, edge: EdgeId, major_conflict: bool) -> bool {
        let capacity = scaled_capacity(
            self.network.edges[edge.index()].capacity_veh_per_s,
            edge,
            &self.scenario,
        );
        if capacity <= 1e-12 {
            return false;
        }
        let node = self.network.edges[edge.index()].to;
        match self.node_mode(node) {
            NodeControl::Fixed => self.phase_open(node, edge),
            NodeControl::Priority => self.is_major(node, edge) || !major_conflict,
        }
    }

    fn node_mode(&self, node: NodeId) -> NodeControl {
        let Some(signal_id) = self.network.nodes[node.index()].signal else {
            return NodeControl::Priority;
        };
        let signal = &self.network.signals[signal_id.index()];
        let scheduled = signal.mode_at(self.time_of_day());
        match self.scenario.forced_mode(scheduled) {
            SignalMode::FixedCycle => NodeControl::Fixed,
            SignalMode::FlashingAmber | SignalMode::Off => NodeControl::Priority,
        }
    }

    fn phase_open(&self, node: NodeId, edge: EdgeId) -> bool {
        let Some(signal_id) = self.network.nodes[node.index()].signal else {
            return true;
        };
        let signal = &self.network.signals[signal_id.index()];
        if signal.cycle_length_s <= 0.0 || signal.phases.is_empty() {
            return false;
        }
        let time = self.time_s + self.clock_origin_s;
        let mut local = (time + signal.offset_s).rem_euclid(signal.cycle_length_s);
        for phase in &signal.phases {
            let moving = phase.green_s + phase.yellow_s;
            let span = moving + phase.all_red_s;
            if local < span {
                return local < moving && phase.incoming.contains(&edge);
            }
            local -= span;
        }
        false
    }

    fn priority_conflict(&self, node: NodeId, t1: f64) -> bool {
        if !matches!(self.node_mode(node), NodeControl::Priority) {
            return false;
        }
        self.network.adjacency.incoming(node).iter().any(|edge| {
            self.is_major(node, *edge) && self.front_ready(*edge, node, t1, false)
        })
    }

    fn front_ready(&self, edge: EdgeId, node: NodeId, t1: f64, minor: bool) -> bool {
        let Some(id) = self.queues.queues[edge.index()].front() else {
            return false;
        };
        let vehicle = &self.vehicles[*id as usize];
        let mut ready = self.ready_time(vehicle, edge, node);
        if minor {
            ready += self.tau(node);
        }
        ready <= t1
    }

    fn ready_time(&self, vehicle: &Vehicle, edge: EdgeId, node: NodeId) -> f64 {
        let mut ready = vehicle.enter_s + self.travel_time(edge);
        if matches!(self.node_mode(node), NodeControl::Priority) && !self.is_major(node, edge) {
            ready += self.tau(node);
        }
        ready
    }

    fn travel_time(&self, edge: EdgeId) -> f64 {
        self.free_flow[edge.index()]
    }

    fn tau(&self, node: NodeId) -> f64 {
        match self.network.nodes[node.index()].signal {
            Some(id) => self.network.signals[id.index()].tau_yield_s,
            None => TAU_YIELD_S,
        }
    }

    fn is_major(&self, node: NodeId, edge: EdgeId) -> bool {
        let max_rank = self
            .network
            .adjacency
            .incoming(node)
            .iter()
            .map(|incoming| self.network.edges[incoming.index()].class.rank())
            .max()
            .unwrap_or(0);
        self.network.edges[edge.index()].class.rank() == max_rank
    }

    fn turn_ok(&self, node: NodeId, from: EdgeId, to: EdgeId) -> bool {
        self.network
            .turns_at(node)
            .iter()
            .any(|turn| turn.from_edge == from && turn.to_edge == to && turn.allowed)
    }

    fn time_of_day(&self) -> u32 {
        (self.clock_origin_s + self.time_s).rem_euclid(86_400.0) as u32
    }
}

struct EntryPlan {
    edge: EdgeId,
    path: Vec<EdgeId>,
    baseline_s: f64,
    habitual_s: f64,
}

struct NextPlan {
    next: Option<EdgeId>,
    path: Vec<EdgeId>,
    cursor: usize,
    baseline_s: f64,
}

enum NodeControl {
    Fixed,
    Priority,
}

fn seconds_until_window_boundary(tod: u32, window: TimeWindow) -> u32 {
    let day = 86_400u32;
    let now = tod % day;
    let ahead = |boundary: u32| -> u32 {
        let boundary = boundary % day;
        if boundary > now {
            boundary - now
        } else {
            day - now + boundary
        }
    };
    ahead(window.start_s).min(ahead(window.end_s)).max(1)
}

fn open_intervals(signal: &SignalController, edge: EdgeId) -> Vec<(f64, f64)> {
    let mut cursor = 0.0;
    let mut intervals = Vec::new();
    for phase in &signal.phases {
        let moving = phase.green_s + phase.yellow_s;
        let span = moving + phase.all_red_s;
        if moving > 0.0 && phase.incoming.contains(&edge) {
            intervals.push((cursor, cursor + moving));
        }
        cursor += span;
    }
    intervals
}

fn local_is_open(local: f64, intervals: &[(f64, f64)]) -> bool {
    intervals.iter().any(|(start, end)| local >= *start && local < *end)
}

fn discharge_slots(open: bool, capacity: f64, credit: f64, dt: f64) -> (u32, f64) {
    if !open || capacity <= 0.0 || dt <= 0.0 {
        return (0, credit);
    }
    let offered = credit + capacity * dt;
    let slots = offered.floor();
    let remainder = offered - slots;
    (slots as u32, remainder)
}
