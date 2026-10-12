use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

use sim_core::{scale_factors, EdgeId, Network, NodeId, Scenario};

use crate::engine::LinkQueues;

/// Drivers recalculate only when experienced remaining time exceeds `inertia * baseline`.
pub fn should_reroute(experienced_s: f64, baseline_s: f64, inertia: f64) -> bool {
    baseline_s.is_finite() && baseline_s > 0.0 && experienced_s > inertia * baseline_s
}

pub fn free_flow_weights(network: &Network, scenario: &Scenario) -> Vec<f64> {
    let (_, speed_factor) = scale_factors(scenario, network.edges.len());
    network
        .edges
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            let speed = edge.free_flow_speed_mps * speed_factor[index];
            if speed <= 1e-9 || edge.storage_max_veh < 1.0 {
                f64::INFINITY
            } else {
                edge.length_m / speed
            }
        })
        .collect()
}

/// Instantaneous traversal cost. An empty open link costs its free-flow time.
/// A queue costs the discharge time of the vehicles already on it when that is longer.
/// A closed link (`q = 0`) is not a finite route.
pub fn experienced_weights(
    network: &Network,
    scenario: &Scenario,
    queues: &LinkQueues,
) -> Vec<f64> {
    let (capacity_factor, speed_factor) = scale_factors(scenario, network.edges.len());
    network
        .edges
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            let speed = edge.free_flow_speed_mps * speed_factor[index];
            let capacity = edge.capacity_veh_per_s * capacity_factor[index];
            if speed <= 1e-9 || capacity <= 1e-12 || edge.storage_max_veh < 1.0 {
                return f64::INFINITY;
            }
            let free_flow = edge.length_m / speed;
            let discharge = queues.vehicles(EdgeId(index as u32)) as f64 / capacity;
            free_flow.max(discharge)
        })
        .collect()
}

pub fn path_cost(path: &[EdgeId], weights: &[f64]) -> f64 {
    path.iter().map(|edge| weights[edge.index()]).sum()
}

/// Turn-aware Dijkstra. The priority queue has no decrease-key, so a node may be
/// pushed more than once; a popped entry whose cost is worse than the recorded
/// distance is discarded immediately. Distances live in a thread-local scratchpad
/// stamped by an epoch, so a search does not allocate or clear those arrays.
pub fn shortest_path(
    network: &Network,
    weights: &[f64],
    from: NodeId,
    to: NodeId,
) -> Option<(Vec<EdgeId>, f64)> {
    if from == to {
        return Some((Vec::new(), 0.0));
    }
    if from.index() >= network.nodes.len() || to.index() >= network.nodes.len() {
        return None;
    }
    let count = network.nodes.len();
    let stamp = explore(network, weights, from, Some(to.index()))?;
    SCRATCH.with(|cell| {
        let scratch = &cell.borrow();
        if scratch.seen[to.index()] != stamp || !scratch.dist[to.index()].is_finite() {
            return None;
        }
        let travelled = scratch.dist[to.index()];
        let mut edges = Vec::new();
        let mut node = to.index();
        for _ in 0..count {
            if node == from.index() {
                break;
            }
            let edge = scratch.prev_edge[node];
            if edge == u32::MAX {
                return None;
            }
            edges.push(EdgeId(edge));
            node = scratch.prev_node[node] as usize;
        }
        if node != from.index() {
            return None;
        }
        edges.reverse();
        Some((edges, travelled))
    })
}

/// Nodes this router can reach from `from`, including `from`. `bits` is a bitset of node indices.
pub fn mark_reached(network: &Network, weights: &[f64], from: NodeId, bits: &mut [u64]) {
    if from.index() >= network.nodes.len() {
        return;
    }
    let Some(stamp) = explore(network, weights, from, None) else {
        return;
    };
    SCRATCH.with(|cell| {
        let scratch = &cell.borrow();
        for (index, seen) in scratch.seen.iter().enumerate().take(network.nodes.len()) {
            if *seen == stamp && scratch.dist[index].is_finite() {
                let word = index / 64;
                if word < bits.len() {
                    bits[word] |= 1_u64 << (index % 64);
                }
            }
        }
    });
}

fn explore(network: &Network, weights: &[f64], from: NodeId, goal: Option<usize>) -> Option<u32> {
    let count = network.nodes.len();
    SCRATCH.with(|cell| {
        let scratch = &mut *cell.borrow_mut();
        scratch.ensure(count);
        scratch.bind(network);
        scratch.stamp = scratch.stamp.wrapping_add(1);
        if scratch.stamp == 0 {
            scratch.seen.fill(0);
            scratch.settled.fill(0);
            scratch.stamp = 1;
        }
        let stamp = scratch.stamp;
        scratch.heap.clear();
        touch(scratch, from.index(), stamp);
        scratch.dist[from.index()] = 0.0;
        scratch.prev_edge[from.index()] = u32::MAX;
        scratch.heap.push(HeapItem {
            cost: 0.0,
            node: from.0,
        });
        while let Some(item) = scratch.heap.pop() {
            let index = item.node as usize;
            if item.cost > scratch.dist[index] {
                continue;
            }
            if scratch.settled[index] == stamp {
                continue;
            }
            scratch.settled[index] = stamp;
            if goal == Some(index) {
                break;
            }
            let travelled = scratch.dist[index];
            let arrived = scratch.prev_edge[index];
            if arrived == u32::MAX {
                for edge in network.adjacency.outgoing(NodeId(item.node)) {
                    relax(scratch, weights, stamp, travelled, item.node, edge.0);
                }
            } else {
                let start = scratch.turn_offset[arrived as usize] as usize;
                let end = scratch.turn_offset[arrived as usize + 1] as usize;
                for slot in start..end {
                    let edge_id = scratch.turn_next[slot];
                    relax(scratch, weights, stamp, travelled, item.node, edge_id);
                }
            }
        }
        Some(stamp)
    })
}

#[inline(always)]
fn touch(scratch: &mut Scratch, node: usize, stamp: u32) {
    if scratch.seen[node] != stamp {
        scratch.seen[node] = stamp;
        scratch.dist[node] = f64::INFINITY;
    }
}

#[inline(always)]
fn relax(
    scratch: &mut Scratch,
    weights: &[f64],
    stamp: u32,
    travelled: f64,
    from: u32,
    edge_id: u32,
) {
    let edge_index = edge_id as usize;
    let weight = weights[edge_index];
    if !weight.is_finite() {
        return;
    }
    let next = scratch.head[edge_index] as usize;
    if scratch.settled[next] == stamp {
        return;
    }
    let proposed = travelled + weight;
    touch(scratch, next, stamp);
    if proposed < scratch.dist[next] {
        scratch.dist[next] = proposed;
        scratch.prev_edge[next] = edge_id;
        scratch.prev_node[next] = from;
        scratch.heap.push(HeapItem {
            cost: proposed,
            node: next as u32,
        });
    }
}

#[derive(Clone, Copy)]
struct HeapItem {
    cost: f64,
    node: u32,
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && self.cost.total_cmp(&other.cost) == Ordering::Equal
    }
}

impl Eq for HeapItem {}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.node.cmp(&self.node))
    }
}

struct Scratch {
    dist: Vec<f64>,
    prev_edge: Vec<u32>,
    prev_node: Vec<u32>,
    seen: Vec<u32>,
    settled: Vec<u32>,
    stamp: u32,
    heap: BinaryHeap<HeapItem>,
    key: usize,
    head: Vec<u32>,
    degree: Vec<u32>,
    turn_offset: Vec<u32>,
    turn_next: Vec<u32>,
}

impl Scratch {
    fn ensure(&mut self, count: usize) {
        if self.dist.len() < count {
            self.dist.resize(count, f64::INFINITY);
            self.prev_edge.resize(count, u32::MAX);
            self.prev_node.resize(count, u32::MAX);
            self.seen.resize(count, 0);
            self.settled.resize(count, 0);
        }
    }

    /// Compact head-node and allowed-turn tables. Built once per network, not per search.
    fn bind(&mut self, network: &Network) {
        let key = network as *const Network as usize;
        if self.key == key && self.head.len() == network.edges.len() {
            return;
        }
        let edges = network.edges.len();
        self.head.clear();
        self.head.reserve(edges);
        for edge in &network.edges {
            self.head.push(edge.to.0);
        }
        self.degree.clear();
        self.degree.resize(edges, 0);
        for turn in &network.turns {
            if turn.allowed {
                let from = turn.from_edge.index();
                if from < edges {
                    self.degree[from] += 1;
                }
            }
        }
        self.turn_offset.clear();
        self.turn_offset.resize(edges + 1, 0);
        for index in 0..edges {
            self.turn_offset[index + 1] = self.turn_offset[index] + self.degree[index];
        }
        let total = self.turn_offset[edges] as usize;
        self.turn_next.clear();
        self.turn_next.resize(total, 0);
        self.degree.fill(0);
        for turn in &network.turns {
            if !turn.allowed {
                continue;
            }
            let from = turn.from_edge.index();
            if from >= edges {
                continue;
            }
            let slot = (self.turn_offset[from] + self.degree[from]) as usize;
            self.turn_next[slot] = turn.to_edge.0;
            self.degree[from] += 1;
        }
        self.key = key;
    }

    fn retained_bytes(&self) -> u64 {
        fn vec_bytes<T>(values: &Vec<T>) -> u64 {
            values.capacity() as u64 * std::mem::size_of::<T>() as u64
        }
        vec_bytes(&self.dist)
            + vec_bytes(&self.prev_edge)
            + vec_bytes(&self.prev_node)
            + vec_bytes(&self.seen)
            + vec_bytes(&self.settled)
            + vec_bytes(&self.head)
            + vec_bytes(&self.degree)
            + vec_bytes(&self.turn_offset)
            + vec_bytes(&self.turn_next)
            + self.heap.len() as u64 * std::mem::size_of::<HeapItem>() as u64
    }
}

/// Bytes of the Dijkstra scratchpad on the calling thread.
/// The binary heap is counted by length, because its spare capacity is not exposed.
pub fn local_router_scratch_bytes() -> u64 {
    SCRATCH.with(|cell| cell.borrow().retained_bytes())
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = const { RefCell::new(Scratch {
        dist: Vec::new(),
        prev_edge: Vec::new(),
        prev_node: Vec::new(),
        seen: Vec::new(),
        settled: Vec::new(),
        stamp: 0,
        heap: BinaryHeap::new(),
        key: 0,
        head: Vec::new(),
        degree: Vec::new(),
        turn_offset: Vec::new(),
        turn_next: Vec::new(),
    }) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inertia_keeps_a_mild_delay_and_rejects_a_large_one() {
        assert!(!should_reroute(48.0, 40.0, 1.4));
        assert!(should_reroute(57.0, 40.0, 1.4));
        assert!(!should_reroute(57.0, 40.0, 1.5));
        assert!(!should_reroute(f64::INFINITY, 0.0, 1.4));
    }

    #[test]
    fn thessaloniki_dijkstra_finishes_within_two_milliseconds() {
        use std::path::Path;
        use std::time::Instant;

        let cache = [
            "data/cache/thessaloniki.simnet",
            "../data/cache/thessaloniki.simnet",
            "../../data/cache/thessaloniki.simnet",
        ]
        .into_iter()
        .map(Path::new)
        .find(|path| path.exists())
        .expect("compiled Thessaloniki cache");
        let network = sim_core::load_network(cache).expect("load cache");
        let weights = free_flow_weights(&network, &sim_core::Scenario::baseline("search-bench"));
        let origins = crate::demand::entry_gateways(&network);
        let destinations = crate::demand::exit_gateways(&network);
        let from = origins[0];
        let mut destination = None;
        for dest in destinations.iter().rev() {
            if let Some((path, _)) = shortest_path(&network, &weights, from, *dest) {
                if path.len() > 20 {
                    destination = Some(*dest);
                    break;
                }
            }
        }
        let destination = destination.expect("a long free-flow path");
        let started = Instant::now();
        let (path, cost) =
            shortest_path(&network, &weights, from, destination).expect("repeat search");
        let elapsed = started.elapsed();
        eprintln!(
            "thessaloniki search  {} edges  {:.1} s  {:.3} ms",
            path.len(),
            cost,
            elapsed.as_secs_f64() * 1_000.0
        );
        assert!(
            cfg!(debug_assertions) || elapsed.as_secs_f64() <= 0.002,
            "search took {:.3} ms",
            elapsed.as_secs_f64() * 1_000.0
        );
    }

    #[test]
    fn giant_component_pairs_have_a_free_flow_path() {
        use std::path::Path;

        let cache = [
            "data/cache/thessaloniki.simnet",
            "../data/cache/thessaloniki.simnet",
            "../../data/cache/thessaloniki.simnet",
        ]
        .into_iter()
        .map(Path::new)
        .find(|path| path.exists())
        .expect("compiled Thessaloniki cache");
        let network = sim_core::load_network(cache).expect("load cache");
        let scenario = sim_core::Scenario::baseline("component");
        let boundary = crate::demand::boundary_demand(&network, &scenario);
        assert!(
            boundary.giant_nodes > 10_000,
            "giant component {}",
            boundary.giant_nodes
        );
        assert!(!boundary.origins.is_empty());
        assert!(!boundary.destinations.is_empty());
        let weights = free_flow_weights(&network, &scenario);
        let mut kilometres = 0.0;
        let mut hours = 0.0;
        let mut checked = 0_usize;
        for origin in boundary.origins.iter().step_by(11) {
            for destination in boundary.destinations.iter().rev().take(6) {
                if !boundary.allows(*origin, *destination) {
                    assert!(shortest_path(&network, &weights, *origin, *destination).is_none());
                    continue;
                }
                let (path, cost) = shortest_path(&network, &weights, *origin, *destination)
                    .expect("routable pair");
                assert_eq!(
                    network.edges[path.last().expect("edge").index()].to,
                    *destination
                );
                for pair in path.windows(2) {
                    let node = network.edges[pair[0].index()].to;
                    let legal = network.turns_at(node).iter().any(|turn| {
                        turn.from_edge == pair[0] && turn.to_edge == pair[1] && turn.allowed
                    });
                    assert!(legal, "illegal turn at {}", node.0);
                }
                let metres: f64 = path
                    .iter()
                    .map(|edge| network.edges[edge.index()].length_m)
                    .sum();
                kilometres += metres / 1000.0;
                hours += cost / 3600.0;
                checked += 1;
            }
        }
        let speed = kilometres / hours;
        eprintln!("free-flow sample {checked} trips  {speed:.1} km/h");
        assert!(checked > 10);
        assert!(
            (20.0..=70.0).contains(&speed),
            "free-flow speed {speed:.1} km/h"
        );
    }

    #[test]
    fn a_handful_of_routable_trips_leave_the_network() {
        use std::path::Path;

        let cache = [
            "data/cache/thessaloniki.simnet",
            "../data/cache/thessaloniki.simnet",
            "../../data/cache/thessaloniki.simnet",
        ]
        .into_iter()
        .map(Path::new)
        .find(|path| path.exists())
        .expect("compiled Thessaloniki cache");
        let network = sim_core::load_network(cache).expect("load cache");
        let scenario = sim_core::Scenario::baseline("finish");
        let boundary = crate::demand::boundary_demand(&network, &scenario);
        let mut trips = Vec::new();
        for (index, origin) in boundary.origins.iter().enumerate().take(8) {
            let destination = boundary
                .destinations
                .iter()
                .rev()
                .find(|destination| boundary.allows(*origin, **destination))
                .copied()
                .expect("exit");
            trips.push(crate::Trip::new(index as f64 * 5.0, *origin, destination));
        }
        let mut sim = crate::Simulation::new(network, scenario);
        sim.schedule(trips);
        sim.advance(7_200.0, 1.0).expect("advance");
        let census = sim.census();
        let speed = if sim.vht_s() > 1.0 {
            sim.vkt_m() * 3.6 / sim.vht_s()
        } else {
            0.0
        };
        eprintln!(
            "sample finished {} of {}  vht {:.2} h  vkt {:.2} km  {:.1} km/h",
            census.completed,
            census.spawned,
            sim.vht_s() / 3600.0,
            sim.vkt_m() / 1000.0,
            speed
        );
        assert_eq!(census.completed, census.spawned);
        assert!(
            (20.0..=45.0).contains(&speed),
            "sample speed {speed:.1} km/h"
        );
    }
}
