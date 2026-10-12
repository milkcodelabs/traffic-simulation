use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use sim_core::{
    scaled_capacity, scaled_speed, storage_inflow_ceiling, EdgeId, LinkTransmissionState, Network, NodeId, Scenario,
    SignalController, SignalMode, TAU_YIELD_S,
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
    /// Simulation time of the last alternate-path search. A blocked driver does not search again immediately.
    route_checked_s: f64,
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
    #[error("{what} became non-finite")]
    NonFinite { what: &'static str },
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
    route_checks: Vec<u64>,
    kept_paths: Vec<(u64, Vec<EdgeId>, f64, f64)>,
    /// Nanoseconds spent inside shortest-path search while building this proposal.
    route_ns: u128,
    /// Nanoseconds spent settling signal discharge credits.
    signal_ns: u128,
    searches: u32,
    reroutes: u32,
}

/// Stage timers for one window of `step` calls.
///
/// `link_ns` includes serial apply. `apply_ns` is that serial portion alone.
/// Signal time is the discharge-credit evaluation inside each busy node, which is
/// how this stepper advances signals: there is no separate controller sweep.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepTimings {
    pub routing_ns: u128,
    pub link_ns: u128,
    pub apply_ns: u128,
    pub signals_ns: u128,
    pub recording_ns: u128,
    pub ticks: u64,
    pub busy_ticks: u64,
    /// Sum of busy-node counts over `busy_ticks`.
    pub busy_node_visits: u64,
}

/// Retained buffers of one running stepper, counted from allocator capacities.
#[derive(Clone, Copy, Debug, Default)]
pub struct BufferFootprint {
    pub vehicle_records: u64,
    pub vehicle_paths: u64,
    pub link_queues: u64,
    pub waiting: u64,
    pub edge_state: u64,
    pub routing_scratch: u64,
    pub pending_trips: u64,
}

impl BufferFootprint {
    pub fn total(self) -> u64 {
        self.vehicle_records
            + self.vehicle_paths
            + self.link_queues
            + self.waiting
            + self.edge_state
            + self.routing_scratch
            + self.pending_trips
    }
}

/// Accumulates for the whole run. A one-line summary is printed at 1,000 ticks
/// unless the caller asked for quiet timings.
#[derive(Clone, Copy, Default)]
struct StepProfile {
    routing_ns: u128,
    weights_ns: u128,
    link_ns: u128,
    apply_ns: u128,
    signals_ns: u128,
    recording_ns: u128,
    ticks: u64,
    busy_ticks: u64,
    busy_node_visits: u64,
    printed: bool,
    quiet: bool,
}

/// Per-edge record of vehicles that have left the link. An unobserved speed is NaN.
#[derive(Clone, Copy, Debug)]
pub struct EdgeObservation {
    pub delay_s: f64,
    pub mean_speed_mps: f64,
}

impl EdgeObservation {
    pub fn observed(self) -> bool {
        self.mean_speed_mps.is_finite()
    }
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
    pub edge_delay_s: Vec<f64>,
    pub edge_travel_s: Vec<f64>,
    pub edge_distance_m: Vec<f64>,
    /// Simulation time through which `discharge_credit` has been brought forward.
    /// Edges that nobody has tried to leave stay behind the clock and catch up on the next discharge.
    pub credit_at: Vec<f64>,
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
    edge_delay_s: Vec<f64>,
    edge_travel_s: Vec<f64>,
    edge_distance_m: Vec<f64>,
    /// Bumps when a vehicle enters or leaves a link, which is when the drawn speeds can change.
    appearance: u64,
    /// Time through which each link's discharge remainder is current.
    credit_at: Vec<f64>,
    /// Traversal cost used by rerouting. Rebuilt for an edge only when its queue length changes.
    experienced: Vec<f64>,
    /// Edge indexes whose queues are non-empty.
    occupied: Vec<u32>,
    occupied_pos: Vec<u32>,
    /// Node indexes with at least one waiting vehicle.
    active_waiting: Vec<u32>,
    waiting_pos: Vec<u32>,
    /// Edges that emptied during the current apply and still need a conservation check.
    just_emptied: Vec<u32>,
    demand: Vec<u32>,
    demand_touched: Vec<u32>,
    busy_ids: Vec<u32>,
    step_profile: StepProfile,
    route_searches: u64,
    reroute_searches: u64,
    rejected_trips: u64,
    /// Set when the scenario is opened for mutation. The next step rebuilds route weights
    /// so a closure is visible to path search on that step, not only to discharge.
    routing_dirty: bool,
    /// Highest `N / S_max` seen on any occupied edge.
    max_density: f64,
    /// Most edges that were at storage in a single step.
    max_full_edges: u32,
    /// Longest spatial queue observed, in vehicles.
    max_queue: u32,
    /// Most vehicles waiting to enter an edge that was already at storage.
    max_spillback: u32,
    /// Edge indexes at storage during the latest saturation sample. Reused so the sample does not allocate.
    full_ids: Vec<u32>,
}

/// A driver who already searched does not search again until this many seconds have passed.
const REROUTE_COOLDOWN_S: f64 = 90.0;
/// Trips with less than this much path left stay on the current sequence of links.
const REROUTE_MIN_REMAINING_M: f64 = 300.0;

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
        let experienced = experienced_weights(&network, &scenario, &LinkQueues::new(edges));
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
            edge_delay_s: vec![0.0; edges],
            edge_travel_s: vec![0.0; edges],
            edge_distance_m: vec![0.0; edges],
            appearance: 0,
            credit_at: vec![0.0; edges],
            experienced,
            occupied: Vec::new(),
            occupied_pos: vec![u32::MAX; edges],
            active_waiting: Vec::new(),
            waiting_pos: vec![u32::MAX; nodes],
            just_emptied: Vec::new(),
            demand: vec![0; edges],
            demand_touched: Vec::new(),
            busy_ids: Vec::new(),
            step_profile: StepProfile::default(),
            route_searches: 0,
            reroute_searches: 0,
            rejected_trips: 0,
            routing_dirty: false,
            max_density: 0.0,
            max_full_edges: 0,
            max_queue: 0,
            max_spillback: 0,
            full_ids: Vec::new(),
        }
    }

    pub fn restore(network: Arc<Network>, snap: SimSnapshot) -> Self {
        let edges = snap.queues.len();
        let nodes = snap.waiting.len();
        let mut sim = Self {
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
            edge_delay_s: snap.edge_delay_s,
            edge_travel_s: snap.edge_travel_s,
            edge_distance_m: snap.edge_distance_m,
            appearance: 0,
            credit_at: snap.credit_at,
            experienced: Vec::new(),
            occupied: Vec::new(),
            occupied_pos: vec![u32::MAX; edges],
            active_waiting: Vec::new(),
            waiting_pos: vec![u32::MAX; nodes],
            just_emptied: Vec::new(),
            demand: vec![0; edges],
            demand_touched: Vec::new(),
            busy_ids: Vec::new(),
            step_profile: StepProfile::default(),
            route_searches: 0,
            reroute_searches: 0,
            rejected_trips: 0,
            routing_dirty: false,
            max_density: 0.0,
            max_full_edges: 0,
            max_queue: 0,
            max_spillback: 0,
            full_ids: Vec::new(),
        };
        sim.rebuild_routing_state();
        sim
    }

    /// Changes only when link contents change. The desktop thread uses it to avoid
    /// rebuilding an identical congestion picture.
    pub fn appearance(&self) -> u64 {
        self.appearance
    }

    pub fn invalidate_picture(&mut self) {
        self.appearance = self.appearance.wrapping_add(1);
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
            edge_delay_s: self.edge_delay_s.clone(),
            edge_travel_s: self.edge_travel_s.clone(),
            edge_distance_m: self.edge_distance_m.clone(),
            credit_at: self.credit_at.clone(),
        }
    }

    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    pub fn free_flow_mps(&self, index: usize) -> f64 {
        let travel = self.free_flow.get(index).copied().unwrap_or(f64::INFINITY);
        let length = self.network.edges.get(index).map(|edge| edge.length_m).unwrap_or(0.0);
        if travel.is_finite() && travel > 1e-9 {
            length / travel
        } else {
            0.0
        }
    }

    pub fn recompute_free_flow(&mut self) {
        self.free_flow = free_flow_weights(&self.network, &self.scenario);
        self.experienced = experienced_weights(&self.network, &self.scenario, &self.queues);
    }

    fn rebuild_routing_state(&mut self) {
        self.experienced = experienced_weights(&self.network, &self.scenario, &self.queues);
        self.occupied.clear();
        self.active_waiting.clear();
        for (index, queue) in self.queues.queues.iter().enumerate() {
            if !queue.is_empty() {
                self.occupied_pos[index] = self.occupied.len() as u32;
                self.occupied.push(index as u32);
            }
        }
        for (index, bucket) in self.waiting.iter().enumerate() {
            if !bucket.is_empty() {
                self.waiting_pos[index] = self.active_waiting.len() as u32;
                self.active_waiting.push(index as u32);
            }
        }
    }

    pub fn observations(&self) -> Vec<EdgeObservation> {
        self.edge_travel_s
            .iter()
            .enumerate()
            .map(|(index, travel)| {
                let distance = self.edge_distance_m[index];
                EdgeObservation {
                    delay_s: self.edge_delay_s[index],
                    mean_speed_mps: if *travel > 1e-9 { distance / travel } else { f64::NAN },
                }
            })
            .collect()
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    pub fn scenario_mut(&mut self) -> &mut Scenario {
        self.routing_dirty = true;
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

    pub fn route_searches(&self) -> u64 {
        self.route_searches
    }

    pub fn reroute_searches(&self) -> u64 {
        self.reroute_searches
    }

    pub fn rejected_trips(&self) -> u64 {
        self.rejected_trips
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
        let on_links = self
            .occupied
            .iter()
            .map(|edge| self.queues.queues[*edge as usize].len() as u64)
            .sum();
        let waiting = self
            .active_waiting
            .iter()
            .map(|node| self.waiting[*node as usize].len() as u64)
            .sum();
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
        if self.routing_dirty {
            self.recompute_free_flow();
            self.routing_dirty = false;
        }
        let t1 = self.time_s + dt;
        let trip_due = self.pending.first().is_some_and(|trip| trip.depart_s <= t1);
        if !trip_due && self.spawned == self.completed {
            self.time_s = t1;
            self.record_profile(0, 0, 0, 0, 0, 0, 0, false);
            return Ok(());
        }
        let clock = Instant::now();
        self.release(t1);
        let mut routing_ns = clock.elapsed().as_nanos();
        let active_during = self.spawned.saturating_sub(self.completed) as f64;
        let clock = Instant::now();
        self.collect_busy(t1);
        let mut link_ns = clock.elapsed().as_nanos();
        if self.busy_ids.is_empty() {
            self.time_s = t1;
            self.vht_s += active_during * dt;
            self.record_profile(routing_ns, 0, link_ns, 0, 0, 0, 0, false);
            return Ok(());
        }
        let nodes = self.busy_ids.clone();
        let clock = Instant::now();
        let proposals: Vec<NodeProposal> = if nodes.len() < 64 {
            nodes.iter().map(|node| self.propose(*node as usize, dt, t1)).collect()
        } else {
            nodes.par_iter().map(|node| self.propose(*node as usize, dt, t1)).collect()
        };
        let propose_ns = clock.elapsed().as_nanos();
        let route_ns: u128 = proposals.iter().map(|proposal| proposal.route_ns).sum();
        let signal_ns: u128 = proposals.iter().map(|proposal| proposal.signal_ns).sum();
        self.route_searches += proposals.iter().map(|proposal| u64::from(proposal.searches)).sum::<u64>();
        self.reroute_searches += proposals.iter().map(|proposal| u64::from(proposal.reroutes)).sum::<u64>();
        routing_ns += route_ns;
        link_ns += propose_ns.saturating_sub(route_ns.saturating_add(signal_ns));
        let clock = Instant::now();
        self.apply(&proposals, t1);
        let apply_ns = clock.elapsed().as_nanos();
        link_ns += apply_ns;
        let clock = Instant::now();
        self.time_s = t1;
        self.vht_s += active_during * dt;
        let result = self.audit();
        self.note_saturation();
        let recording_ns = clock.elapsed().as_nanos();
        self.record_profile(
            routing_ns,
            0,
            link_ns,
            signal_ns,
            recording_ns,
            apply_ns,
            nodes.len() as u32,
            true,
        );
        result
    }

    pub fn max_density(&self) -> f64 {
        self.max_density
    }

    pub fn max_full_edges(&self) -> u32 {
        self.max_full_edges
    }

    pub fn max_queue(&self) -> u32 {
        self.max_queue
    }

    pub fn max_spillback(&self) -> u32 {
        self.max_spillback
    }

    /// Suppress the one-shot 1,000-tick profile line. Counters still accumulate.
    pub fn set_profile_quiet(&mut self, quiet: bool) {
        self.step_profile.quiet = quiet;
    }

    /// Zero the stage timers without forgetting the quiet flag.
    pub fn reset_timings(&mut self) {
        let quiet = self.step_profile.quiet;
        self.step_profile = StepProfile {
            quiet,
            printed: true,
            ..StepProfile::default()
        };
    }

    pub fn timings(&self) -> StepTimings {
        let profile = &self.step_profile;
        StepTimings {
            routing_ns: profile.routing_ns,
            link_ns: profile.link_ns,
            apply_ns: profile.apply_ns,
            signals_ns: profile.signals_ns,
            recording_ns: profile.recording_ns,
            ticks: profile.ticks,
            busy_ticks: profile.busy_ticks,
            busy_node_visits: profile.busy_node_visits,
        }
    }

    /// Bytes retained by the stepper's own buffers, including spare capacity.
    /// Completed vehicles stay in the table, so this grows with departures, not with the vehicles still on the road.
    pub fn buffer_footprint(&self) -> BufferFootprint {
        fn bytes<T>(capacity: usize) -> u64 {
            capacity as u64 * std::mem::size_of::<T>() as u64
        }
        let mut vehicle_paths = 0u64;
        for vehicle in &self.vehicles {
            vehicle_paths += bytes::<EdgeId>(vehicle.path.capacity());
        }
        let mut link_queues = bytes::<VecDeque<u64>>(self.queues.queues.capacity());
        for queue in &self.queues.queues {
            link_queues += bytes::<u64>(queue.capacity());
        }
        let mut waiting = bytes::<Vec<u64>>(self.waiting.capacity());
        for bucket in &self.waiting {
            waiting += bytes::<u64>(bucket.capacity());
        }
        let edge_state = bytes::<f64>(self.discharge_credit.capacity())
            + bytes::<f64>(self.free_flow.capacity())
            + bytes::<f64>(self.edge_delay_s.capacity())
            + bytes::<f64>(self.edge_travel_s.capacity())
            + bytes::<f64>(self.edge_distance_m.capacity())
            + bytes::<f64>(self.credit_at.capacity())
            + bytes::<f64>(self.experienced.capacity())
            + bytes::<f64>(self.links.cumulative_in.capacity())
            + bytes::<f64>(self.links.cumulative_out.capacity())
            + bytes::<f64>(self.links.cumulative_demand.capacity())
            + bytes::<u32>(self.demand.capacity())
            + bytes::<u32>(self.occupied_pos.capacity());
        let routing_scratch = bytes::<u32>(self.occupied.capacity())
            + bytes::<u32>(self.active_waiting.capacity())
            + bytes::<u32>(self.waiting_pos.capacity())
            + bytes::<u32>(self.just_emptied.capacity())
            + bytes::<u32>(self.demand_touched.capacity())
            + bytes::<u32>(self.busy_ids.capacity())
            + bytes::<u32>(self.full_ids.capacity());
        BufferFootprint {
            vehicle_records: bytes::<Vehicle>(self.vehicles.capacity()),
            vehicle_paths,
            link_queues,
            waiting,
            edge_state,
            routing_scratch,
            pending_trips: bytes::<Trip>(self.pending.capacity()),
        }
    }

    /// Records how close the network came to jam density, and how many vehicles
    /// were held upstream of a link that could not take another vehicle.
    fn note_saturation(&mut self) {
        self.full_ids.clear();
        let mut peak = 0.0_f64;
        for &edge in &self.occupied {
            let index = edge as usize;
            let storage = self.network.edges[index].storage_max_veh;
            let queued = self.queues.queues[index].len() as f64;
            let density = if storage > 1e-9 {
                queued / storage
            } else if queued > 0.0 {
                1.0
            } else {
                0.0
            };
            peak = peak.max(density);
            let queued_n = self.queues.queues[index].len() as u32;
            if queued_n > self.max_queue {
                self.max_queue = queued_n;
            }
            // The stepper admits a vehicle only when at least one whole space remains.
            if storage_inflow_ceiling(storage, queued) < 1.0 {
                self.full_ids.push(edge);
            }
        }
        self.full_ids.sort_unstable();
        let mut held = 0u32;
        for &node in &self.active_waiting {
            for id in &self.waiting[node as usize] {
                let Some(edge) = self.vehicles[*id as usize].path.first().copied() else {
                    continue;
                };
                if self.full_ids.binary_search(&(edge.index() as u32)).is_ok() {
                    held += 1;
                }
            }
        }
        if peak > self.max_density {
            self.max_density = peak;
        }
        let full = self.full_ids.len() as u32;
        if full > self.max_full_edges {
            self.max_full_edges = full;
        }
        if held > self.max_spillback {
            self.max_spillback = held;
        }
    }

    fn record_profile(
        &mut self,
        routing_ns: u128,
        weights_ns: u128,
        link_ns: u128,
        signals_ns: u128,
        recording_ns: u128,
        apply_ns: u128,
        busy_nodes: u32,
        busy: bool,
    ) {
        let profile = &mut self.step_profile;
        profile.routing_ns += routing_ns;
        profile.weights_ns += weights_ns;
        profile.link_ns += link_ns;
        profile.apply_ns += apply_ns;
        profile.signals_ns += signals_ns;
        profile.recording_ns += recording_ns;
        profile.ticks += 1;
        if busy {
            profile.busy_ticks += 1;
            profile.busy_node_visits += u64::from(busy_nodes);
        }
        if profile.quiet || profile.printed || profile.ticks < 1000 {
            return;
        }
        profile.printed = true;
        let total = profile.routing_ns + profile.link_ns + profile.signals_ns + profile.recording_ns;
        let seconds = total as f64 / 1e9;
        let rate = 1000.0 / seconds.max(1e-9);
        let search_ns = profile.routing_ns.saturating_sub(profile.weights_ns);
        eprintln!(
            "profile 1000 ticks in {seconds:.3}s  {rate:.1} ticks/s  busy {busy}\n  time_routing {routing:.3}s  (weights {weights:.3}s  searches {search:.3}s)\n  time_link_dynamics {link:.3}s  (apply {apply:.3}s)\n  time_signals {signals:.3}s\n  time_recording {recording:.3}s",
            busy = profile.busy_ticks,
            routing = profile.routing_ns as f64 / 1e9,
            weights = profile.weights_ns as f64 / 1e9,
            search = search_ns as f64 / 1e9,
            link = profile.link_ns as f64 / 1e9,
            apply = profile.apply_ns as f64 / 1e9,
            signals = profile.signals_ns as f64 / 1e9,
            recording = profile.recording_ns as f64 / 1e9,
        );
    }

    /// A node is busy only when a vehicle can enter or leave it during this step.
    /// A vehicle still covering its free-flow time does not make the downstream node busy.
    fn collect_busy(&mut self, t1: f64) {
        self.busy_ids.clear();
        for &node in &self.active_waiting {
            let due = self.waiting[node as usize]
                .iter()
                .any(|id| self.vehicles[*id as usize].depart_s <= t1);
            if due {
                self.busy_ids.push(node);
            }
        }
        for &edge in &self.occupied {
            let Some(id) = self.queues.queues[edge as usize].front().copied() else {
                continue;
            };
            let head = self.network.edges[edge as usize].to;
            if self.ready_time(&self.vehicles[id as usize], EdgeId(edge), head) <= t1 {
                self.busy_ids.push(head.index() as u32);
            }
        }
        self.busy_ids.sort_unstable();
        self.busy_ids.dedup();
    }

    fn push_waiting(&mut self, node: usize, id: u64) {
        if self.waiting[node].is_empty() {
            self.waiting_pos[node] = self.active_waiting.len() as u32;
            self.active_waiting.push(node as u32);
        }
        self.waiting[node].push(id);
    }

    fn unmark_waiting(&mut self, node: usize) {
        let pos = self.waiting_pos[node];
        if pos == u32::MAX {
            return;
        }
        let last = self.active_waiting.pop().expect("waiting mark");
        if last as usize != node {
            self.active_waiting[pos as usize] = last;
            self.waiting_pos[last as usize] = pos;
        }
        self.waiting_pos[node] = u32::MAX;
    }

    fn occupy(&mut self, index: usize) {
        if self.occupied_pos[index] != u32::MAX {
            return;
        }
        self.occupied_pos[index] = self.occupied.len() as u32;
        self.occupied.push(index as u32);
    }

    fn vacate(&mut self, index: usize) {
        let pos = self.occupied_pos[index];
        if pos == u32::MAX {
            return;
        }
        let last = self.occupied.pop().expect("occupied mark");
        if last as usize != index {
            self.occupied[pos as usize] = last;
            self.occupied_pos[last as usize] = pos;
        }
        self.occupied_pos[index] = u32::MAX;
    }

    fn enqueue(&mut self, index: usize, vehicle: u64) {
        let was_empty = self.queues.queues[index].is_empty();
        self.queues.queues[index].push_back(vehicle);
        if was_empty {
            self.occupy(index);
        }
        self.refresh_weight(index);
    }

    fn refresh_weight(&mut self, index: usize) {
        let edge = &self.network.edges[index];
        let id = EdgeId(index as u32);
        let speed = scaled_speed(edge.free_flow_speed_mps, id, &self.scenario);
        let capacity = scaled_capacity(edge.capacity_veh_per_s, id, &self.scenario);
        self.experienced[index] = if speed <= 1e-9 || capacity <= 1e-12 {
            f64::INFINITY
        } else {
            let free = edge.length_m / speed;
            let discharge = self.queues.queues[index].len() as f64 / capacity;
            free.max(discharge)
        };
    }

    fn edge_room(&self, index: usize) -> u32 {
        let edge = &self.network.edges[index];
        let capacity = scaled_capacity(edge.capacity_veh_per_s, EdgeId(index as u32), &self.scenario);
        if capacity <= 1e-12 {
            return 0;
        }
        let occupied = self.queues.queues[index].len() as f64;
        storage_inflow_ceiling(edge.storage_max_veh, occupied).floor() as u32
    }

    /// Discharge remainder an empty approach would hold at `until`, catching up skipped seconds.
    fn credit_settled(&self, index: usize, until: f64) -> f64 {
        let mut credit = self.discharge_credit[index];
        let mut at = self.credit_at[index];
        if until <= at + 1e-12 {
            return credit;
        }
        let capacity = self.edge_capacity(index, self.scenario.edge_capacity_scale.is_empty());
        if capacity <= 1e-12 {
            return credit;
        }
        let edge = EdgeId(index as u32);
        let head = self.network.edges[index].to;
        while at + 1.0 <= until + 1e-9 {
            if self.idle_open(head, edge, at) {
                let offered = credit + capacity;
                credit = offered - offered.floor();
            }
            at += 1.0;
        }
        let frac = until - at;
        if frac > 1e-8 && self.idle_open(head, edge, at) {
            let offered = credit + capacity * frac;
            credit = offered - offered.floor();
        }
        credit
    }

    fn idle_open(&self, node: NodeId, edge: EdgeId, time_s: f64) -> bool {
        match self.control_at(node, time_s) {
            NodeControl::Fixed => self.phase_open_at(node, edge, time_s),
            NodeControl::Priority => true,
        }
    }

    fn control_at(&self, node: NodeId, time_s: f64) -> NodeControl {
        let Some(signal_id) = self.network.nodes[node.index()].signal else {
            return NodeControl::Priority;
        };
        let signal = &self.network.signals[signal_id.index()];
        match self.scenario.forced_mode(self.scheduled_mode_at(signal, time_s)) {
            SignalMode::FixedCycle => NodeControl::Fixed,
            SignalMode::FlashingAmber | SignalMode::Off => NodeControl::Priority,
        }
    }

    fn release(&mut self, t1: f64) {
        let split = self
            .pending
            .iter()
            .position(|trip| trip.depart_s > t1)
            .unwrap_or(self.pending.len());
        if split == 0 {
            return;
        }
        let due: Vec<Trip> = self.pending.drain(..split).collect();
        // Free-flow time, with zero-capacity edges removed. If every path is severed, fall
        // back to the geometric shortest path so the trip still queues instead of vanishing.
        let usable = self.departure_weights();
        for trip in due {
            let id = self.vehicles.len() as u64;
            let inertia = trip.inertia.unwrap_or(self.scenario.route_inertia);
            self.route_searches += 1;
            let preferred = shortest_path(&self.network, &usable, trip.origin, trip.destination);
            let path = if let Some((path, _)) = preferred {
                path
            } else {
                self.route_searches += 1;
                let Some((path, _)) = shortest_path(&self.network, &self.free_flow, trip.origin, trip.destination) else {
                    self.rejected_trips += 1;
                    if self.rejected_trips <= 12 || self.rejected_trips % 1000 == 0 {
                        eprintln!(
                            "rejected unroutable departure {:.0}s {} -> {} ({} so far)",
                            trip.depart_s, trip.origin.0, trip.destination.0, self.rejected_trips
                        );
                    }
                    continue;
                };
                path
            };
            let habitual = path_cost(&path, &self.free_flow);
            if !habitual.is_finite() {
                self.rejected_trips += 1;
                continue;
            }
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
                route_checked_s: f64::NEG_INFINITY,
            });
            self.spawned += 1;
            if trip.origin == trip.destination {
                self.vehicles[id as usize].arrive_s = Some(trip.depart_s);
                self.completed += 1;
            } else {
                self.push_waiting(trip.origin.index(), id);
            }
        }
    }

    fn propose(&self, node_index: usize, dt: f64, t1: f64) -> NodeProposal {
        let node = NodeId(node_index as u32);
        let mut proposal = NodeProposal {
            departures: Vec::new(),
            entries: Vec::new(),
            credits: Vec::new(),
            demand: Vec::new(),
            route_checks: Vec::new(),
            kept_paths: Vec::new(),
            route_ns: 0,
            signal_ns: 0,
            searches: 0,
            reroutes: 0,
        };
        let major_conflict = self.priority_conflict(node, t1);
        let mut incoming: Vec<EdgeId> = self.network.adjacency.incoming(node).to_vec();
        incoming.sort_by_key(|edge| edge.0);
        let mut room = LocalRoom::from_outgoing(self, node);
        for edge in incoming {
            let clock = Instant::now();
            let open = self.approach_open(edge, major_conflict);
            let capacity = scaled_capacity(self.network.edges[edge.index()].capacity_veh_per_s, edge, &self.scenario);
            let settled = self.credit_settled(edge.index(), self.time_s);
            let (slots, credit) = discharge_slots(open, capacity, settled, dt);
            proposal.signal_ns += clock.elapsed().as_nanos();
            proposal.credits.push((edge.index(), credit));
            self.drain_edge(node, edge, slots, t1, &mut room, &mut proposal);
        }
        self.drain_origin(node, t1, &mut room, &mut proposal);
        proposal
    }

    fn drain_edge(
        &self,
        node: NodeId,
        edge: EdgeId,
        mut slots: u32,
        t1: f64,
        room: &mut LocalRoom,
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
            let decision = self.decide_next(vehicle, node);
            proposal.route_ns += decision.route_ns;
            proposal.searches += decision.searches;
            proposal.reroutes += decision.reroutes;
            if decision.searched {
                proposal.route_checks.push(vehicle.id);
            }
            let Some(next) = decision.next else {
                break;
            };
            if !self.turn_ok(node, edge, next) || room.left(next) == 0 {
                if room.left(next) == 0 {
                    let held = self.queues.queues[edge.index()]
                        .iter()
                        .skip(position)
                        .filter(|id| self.ready_time(&self.vehicles[**id as usize], edge, node) <= t1)
                        .count() as u32;
                    proposal.demand.push((next.index(), held));
                }
                break;
            }
            room.take(next);
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

    fn drain_origin(&self, node: NodeId, t1: f64, room: &mut LocalRoom, proposal: &mut NodeProposal) {
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
            let plan = self.decide_entry(vehicle, node);
            proposal.route_ns += plan.route_ns;
            proposal.searches += plan.searches;
            proposal.reroutes += plan.reroutes;
            if plan.searched {
                proposal.route_checks.push(vehicle.id);
            }
            if !plan.path.is_empty() && (vehicle.path.is_empty() || self.network.edges[vehicle.path[0].index()].from != node) {
                proposal.kept_paths.push((vehicle.id, plan.path.clone(), plan.baseline_s, plan.habitual_s));
            }
            plans.push(Some(plan));
        }
        let mut entered = vec![false; waiting.len()];
        let outgoing = self.network.adjacency.outgoing(node);
        for edge in outgoing {
            let mut left = room.left(*edge);
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
            room.set(*edge, left);
        }
    }

    fn decide_entry(&self, vehicle: &Vehicle, node: NodeId) -> EntryPlan {
        let mut path = vehicle.path.clone();
        let mut baseline = vehicle.baseline_s;
        let mut habitual = vehicle.habitual_s;
        let mut searched = false;
        let mut searches = 0u32;
        let mut reroutes = 0u32;
        let mut route_ns = 0u128;
        // An empty path means the free-flow search at release already failed. Repeating
        // it while the vehicle waits does not create a route. A misaligned path is a
        // different node and may be repaired, at most once per cooldown.
        let misaligned = !path.is_empty() && self.network.edges[path[0].index()].from != node;
        if misaligned && self.route_due(vehicle) {
            searched = true;
            searches += 1;
            let clock = Instant::now();
            let found = shortest_path(&self.network, &self.free_flow, node, vehicle.destination);
            route_ns += clock.elapsed().as_nanos();
            if let Some((replacement, cost)) = found {
                path = replacement;
                baseline = cost;
                if !habitual.is_finite() {
                    habitual = cost;
                }
            }
        }
        if self.route_due(vehicle)
            && self.may_reroute(node, &path, 0)
            && should_reroute(path_cost(&path, &self.experienced), path_cost(&path, &self.free_flow), vehicle.inertia)
        {
            searched = true;
            searches += 1;
            reroutes += 1;
            let clock = Instant::now();
            let found = shortest_path(&self.network, &self.experienced, node, vehicle.destination);
            route_ns += clock.elapsed().as_nanos();
            if let Some((replacement, _)) = found {
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
            searched,
            route_ns,
            searches,
            reroutes,
        }
    }

    fn route_due(&self, vehicle: &Vehicle) -> bool {
        self.time_s >= vehicle.route_checked_s + REROUTE_COOLDOWN_S
    }

    fn departure_weights(&self) -> Vec<f64> {
        self.free_flow
            .iter()
            .enumerate()
            .map(|(index, weight)| {
                let capacity = scaled_capacity(
                    self.network.edges[index].capacity_veh_per_s,
                    EdgeId(index as u32),
                    &self.scenario,
                );
                if !weight.is_finite() || capacity <= 1e-12 {
                    f64::INFINITY
                } else {
                    *weight
                }
            })
            .collect()
    }

    /// A queued vehicle cannot leave the link it is already on. A new path is considered
    /// only at a junction with two or more exits. Mild delay is ignored inside the last
    /// 300 m. A severed remaining edge is not mild delay: the driver may leave it at once.
    fn may_reroute(&self, node: NodeId, path: &[EdgeId], from_cursor: usize) -> bool {
        if self.network.adjacency.outgoing(node).len() < 2 {
            return false;
        }
        let mut metres = 0.0;
        for edge in path.iter().skip(from_cursor) {
            if !self.experienced[edge.index()].is_finite() {
                return true;
            }
            metres += self.network.edges[edge.index()].length_m;
            if metres >= REROUTE_MIN_REMAINING_M {
                return true;
            }
        }
        false
    }

    fn decide_next(&self, vehicle: &Vehicle, node: NodeId) -> NextPlan {
        let suffix_start = vehicle.cursor + 1;
        let mut path = vehicle.path.clone();
        let mut baseline = path_cost(&path[suffix_start.min(path.len())..], &self.free_flow);
        let mut searched = false;
        let mut searches = 0u32;
        let mut reroutes = 0u32;
        let mut route_ns = 0u128;
        if self.route_due(vehicle) && self.may_reroute(node, &path, suffix_start) {
            let experienced_cost = path_cost(&path[suffix_start.min(path.len())..], &self.experienced);
            if should_reroute(experienced_cost, baseline, vehicle.inertia) {
                searched = true;
                searches += 1;
                reroutes += 1;
                let clock = Instant::now();
                let found = shortest_path(&self.network, &self.experienced, node, vehicle.destination);
                route_ns += clock.elapsed().as_nanos();
                if let Some((replacement, _)) = found {
                    let old_next = path.get(suffix_start).copied();
                    if replacement.first().copied() != old_next {
                        path.truncate(suffix_start);
                        path.extend(replacement.iter().copied());
                        baseline = path_cost(&replacement, &self.free_flow);
                    }
                }
            }
        }
        let next = path.get(suffix_start).copied();
        let cursor = suffix_start;
        NextPlan {
            next,
            path,
            cursor,
            baseline_s: baseline,
            searched,
            route_ns,
            searches,
            reroutes,
        }
    }

    fn apply(&mut self, proposals: &[NodeProposal], t1: f64) {
        for edge in &self.demand_touched {
            let index = *edge as usize;
            self.demand[index] = 0;
            self.links.cumulative_demand[index] = 0.0;
        }
        self.demand_touched.clear();
        for proposal in proposals {
            for (edge, credit) in &proposal.credits {
                self.discharge_credit[*edge] = *credit;
                self.credit_at[*edge] = t1;
            }
            for (edge, count) in &proposal.demand {
                if self.demand[*edge] == 0 {
                    self.demand_touched.push(*edge as u32);
                }
                self.demand[*edge] = self.demand[*edge].saturating_add(*count);
            }
        }
        for edge in &self.demand_touched {
            self.links.cumulative_demand[*edge as usize] = f64::from(self.demand[*edge as usize]);
        }
        for proposal in proposals {
            for departure in &proposal.departures {
                let edge_index = departure.edge.index();
                let front = self.queues.queues[edge_index]
                    .pop_front()
                    .expect("departure matches the FIFO prefix");
                assert_eq!(front, departure.vehicle);
                if self.queues.queues[edge_index].is_empty() {
                    self.vacate(edge_index);
                    self.just_emptied.push(edge_index as u32);
                }
                self.refresh_weight(edge_index);
                self.links.cumulative_out[edge_index] += 1.0;
                let spent = (departure.at - self.vehicles[departure.vehicle as usize].enter_s).max(0.0);
                let travel = self.free_flow[edge_index];
                let length = self.network.edges[edge_index].length_m;
                if travel.is_finite() {
                    let delay = (spent - travel).max(0.0);
                    self.delay_s += delay;
                    self.edge_delay_s[edge_index] += delay;
                }
                self.edge_travel_s[edge_index] += spent;
                self.edge_distance_m[edge_index] += length;
                self.vkt_m += length;
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
                let from = self.network.edges[entry.edge.index()].from.index();
                self.waiting[from].retain(|id| *id != entry.vehicle);
                if self.waiting[from].is_empty() {
                    self.unmark_waiting(from);
                }
                let vehicle = &mut self.vehicles[entry.vehicle as usize];
                vehicle.path.clone_from(&entry.path);
                vehicle.cursor = 0;
                vehicle.on_link = true;
                vehicle.enter_s = entry.at;
                vehicle.baseline_s = entry.baseline_s;
                if entry.habitual_s.is_finite() {
                    vehicle.habitual_s = entry.habitual_s;
                }
                self.enqueue(entry.edge.index(), entry.vehicle);
                self.links.cumulative_in[entry.edge.index()] += 1.0;
            }
        }
        for proposal in proposals {
            for departure in &proposal.departures {
                let MoveKind::Transfer { to, .. } = &departure.kind else {
                    continue;
                };
                let vehicle = &mut self.vehicles[departure.vehicle as usize];
                vehicle.on_link = true;
                vehicle.enter_s = departure.at;
                self.enqueue(to.index(), departure.vehicle);
                self.links.cumulative_in[to.index()] += 1.0;
            }
        }
        for proposal in proposals {
            for id in &proposal.route_checks {
                self.vehicles[*id as usize].route_checked_s = t1;
            }
            for (id, path, baseline, habitual) in &proposal.kept_paths {
                let vehicle = &mut self.vehicles[*id as usize];
                vehicle.path.clone_from(path);
                vehicle.baseline_s = *baseline;
                if habitual.is_finite() {
                    vehicle.habitual_s = *habitual;
                }
            }
        }
        if proposals.iter().any(|proposal| !proposal.departures.is_empty() || !proposal.entries.is_empty()) {
            self.appearance = self.appearance.wrapping_add(1);
        }
    }

    fn audit(&mut self) -> Result<(), EngineError> {
        if !self.vht_s.is_finite() || !self.vkt_m.is_finite() || !self.delay_s.is_finite() {
            return Err(EngineError::NonFinite { what: "network totals" });
        }
        let census = self.census();
        if census.spawned != census.active + census.completed {
            return Err(EngineError::Conservation {
                spawned: census.spawned,
                active: census.active,
                completed: census.completed,
            });
        }
        for edge in &self.occupied {
            self.check_edge(*edge)?;
        }
        let emptied = std::mem::take(&mut self.just_emptied);
        for edge in emptied {
            if self.occupied_pos[edge as usize] != u32::MAX {
                continue;
            }
            self.check_edge(edge)?;
        }
        Ok(())
    }

    fn check_edge(&self, edge: u32) -> Result<(), EngineError> {
        let index = edge as usize;
        let queued = self.queues.queues[index].len();
        let counted = self.links.vehicles(EdgeId(edge));
        if (counted - queued as f64).abs() > 1e-6 {
            return Err(EngineError::LinkMismatch {
                edge,
                queued,
                counted,
            });
        }
        let storage = self.network.edges[index].storage_max_veh;
        if queued as f64 > storage + 1e-6 {
            return Err(EngineError::Storage {
                edge,
                queued,
                storage,
            });
        }
        if !self.discharge_credit[index].is_finite() {
            return Err(EngineError::NonFinite { what: "discharge credit" });
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
        self.time_s = horizon;
        seconds
    }

    /// Move the clock forward while every vehicle is still traveling and no trip is due.
    /// The second a vehicle becomes ready, or a trip departs, stays with [`Simulation::step`].
    /// Discharge remainders stay where they are and catch up on the next leave.
    pub fn coast(&mut self, max_seconds: u32) -> u32 {
        if max_seconds == 0 {
            return 0;
        }
        let next_trip = self.pending.first().map(|trip| trip.depart_s).unwrap_or(f64::INFINITY);
        let next_ready = self.next_ready_s();
        let horizon = self.time_s + f64::from(max_seconds);
        let stop = next_trip.min(next_ready).min(horizon);
        let mut seconds = (stop - self.time_s).floor().max(0.0) as u32;
        seconds = seconds.min(max_seconds);
        if seconds > 0 {
            let landed = self.time_s + f64::from(seconds);
            if (landed - next_trip).abs() <= 1e-6 || (landed - next_ready).abs() <= 1e-6 {
                seconds -= 1;
            }
        }
        if seconds == 0 {
            return 0;
        }
        let active = self.spawned.saturating_sub(self.completed) as f64;
        self.time_s += f64::from(seconds);
        self.vht_s += active * f64::from(seconds);
        seconds
    }

    fn next_ready_s(&self) -> f64 {
        let mut next = f64::INFINITY;
        for edge in &self.occupied {
            let Some(id) = self.queues.queues[*edge as usize].front() else {
                continue;
            };
            let head = self.network.edges[*edge as usize].to;
            next = next.min(self.ready_time(&self.vehicles[*id as usize], EdgeId(*edge), head));
        }
        for node in &self.active_waiting {
            for id in &self.waiting[*node as usize] {
                next = next.min(self.vehicles[*id as usize].depart_s);
            }
        }
        next
    }

    fn edge_capacity(&self, index: usize, bare: bool) -> f64 {
        let base = self.network.edges[index].capacity_veh_per_s;
        if bare {
            base.max(0.0)
        } else {
            scaled_capacity(base, EdgeId(index as u32), &self.scenario)
        }
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
        self.control_at(node, self.time_s)
    }

    fn scheduled_mode_at(&self, signal: &SignalController, time_s: f64) -> SignalMode {
        let tod = (self.clock_origin_s + time_s).rem_euclid(86_400.0) as u32;
        if let Some(window) = self.scenario.night_flashing {
            if window.contains(tod) {
                signal.night_mode
            } else {
                signal.mode_outside_night
            }
        } else {
            signal.mode_at(tod)
        }
    }

    fn phase_open(&self, node: NodeId, edge: EdgeId) -> bool {
        self.phase_open_at(node, edge, self.time_s)
    }

    fn phase_open_at(&self, node: NodeId, edge: EdgeId, time_s: f64) -> bool {
        let Some(signal_id) = self.network.nodes[node.index()].signal else {
            return true;
        };
        let signal = &self.network.signals[signal_id.index()];
        let listed = signal.phases.iter().any(|phase| phase.incoming.contains(&edge));
        if !listed || signal.cycle_length_s <= 0.0 || signal.phases.is_empty() {
            return true;
        }
        let time = time_s + self.clock_origin_s;
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
}

struct LocalRoom {
    ids: Vec<u32>,
    left: Vec<u32>,
}

impl LocalRoom {
    fn from_outgoing(sim: &Simulation, node: NodeId) -> Self {
        let outgoing = sim.network.adjacency.outgoing(node);
        let mut ids = Vec::with_capacity(outgoing.len());
        let mut left = Vec::with_capacity(outgoing.len());
        for edge in outgoing {
            ids.push(edge.0);
            left.push(sim.edge_room(edge.index()));
        }
        Self { ids, left }
    }

    fn slot(&self, edge: EdgeId) -> Option<usize> {
        self.ids.iter().position(|id| *id == edge.0)
    }

    fn left(&self, edge: EdgeId) -> u32 {
        self.slot(edge).map(|slot| self.left[slot]).unwrap_or(0)
    }

    fn take(&mut self, edge: EdgeId) {
        if let Some(slot) = self.slot(edge) {
            self.left[slot] = self.left[slot].saturating_sub(1);
        }
    }

    fn set(&mut self, edge: EdgeId, value: u32) {
        if let Some(slot) = self.slot(edge) {
            self.left[slot] = value;
        }
    }
}

struct EntryPlan {
    edge: EdgeId,
    path: Vec<EdgeId>,
    baseline_s: f64,
    habitual_s: f64,
    searched: bool,
    route_ns: u128,
    searches: u32,
    reroutes: u32,
}

struct NextPlan {
    next: Option<EdgeId>,
    path: Vec<EdgeId>,
    cursor: usize,
    baseline_s: f64,
    searched: bool,
    route_ns: u128,
    searches: u32,
    reroutes: u32,
}

enum NodeControl {
    Fixed,
    Priority,
}

fn discharge_slots(open: bool, capacity: f64, credit: f64, dt: f64) -> (u32, f64) {
    let credit = if credit.is_finite() && credit >= 0.0 { credit } else { 0.0 };
    if !open || !(capacity > 0.0) || !(dt > 0.0) || !capacity.is_finite() || !dt.is_finite() {
        return (0, credit);
    }
    let offered = credit + capacity * dt;
    if !offered.is_finite() {
        return (0, 0.0);
    }
    let slots = offered.floor();
    let remainder = offered - slots;
    (slots as u32, remainder)
}
