use std::cmp::Ordering;
use std::collections::BinaryHeap;

use sim_core::{scaled_capacity, scaled_speed, EdgeId, Network, NodeId, Scenario};

use crate::engine::LinkQueues;

/// Drivers recalculate only when experienced remaining time exceeds `inertia * baseline`.
pub fn should_reroute(experienced_s: f64, baseline_s: f64, inertia: f64) -> bool {
    baseline_s.is_finite() && baseline_s > 0.0 && experienced_s > inertia * baseline_s
}

pub fn free_flow_weights(network: &Network, scenario: &Scenario) -> Vec<f64> {
    network
        .edges
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            let speed = scaled_speed(edge.free_flow_speed_mps, EdgeId(index as u32), scenario);
            if speed <= 1e-9 {
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
pub fn experienced_weights(network: &Network, scenario: &Scenario, queues: &LinkQueues) -> Vec<f64> {
    network
        .edges
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            let id = EdgeId(index as u32);
            let speed = scaled_speed(edge.free_flow_speed_mps, id, scenario);
            let capacity = scaled_capacity(edge.capacity_veh_per_s, id, scenario);
            if speed <= 1e-9 || capacity <= 1e-12 {
                return f64::INFINITY;
            }
            let free_flow = edge.length_m / speed;
            let discharge = queues.vehicles(id) as f64 / capacity;
            free_flow.max(discharge)
        })
        .collect()
}

pub fn path_cost(path: &[EdgeId], weights: &[f64]) -> f64 {
    path.iter().map(|edge| weights[edge.index()]).sum()
}

/// Turn-respecting Dijkstra. The state is the node just reached.
/// The origin expands every outgoing edge; later nodes expand only `Turn.allowed` pairs.
/// Edges with a non-finite weight are not traversed.
pub fn shortest_path(network: &Network, weights: &[f64], from: NodeId, to: NodeId) -> Option<(Vec<EdgeId>, f64)> {
    if from == to {
        return Some((Vec::new(), 0.0));
    }
    if from.index() >= network.nodes.len() || to.index() >= network.nodes.len() {
        return None;
    }
    let count = network.nodes.len();
    let mut dist = vec![f64::INFINITY; count];
    let mut prev_edge: Vec<Option<EdgeId>> = vec![None; count];
    let mut prev_node: Vec<Option<NodeId>> = vec![None; count];
    dist[from.index()] = 0.0;
    let mut heap = BinaryHeap::new();
    heap.push(HeapNode {
        cost: 0.0,
        node: from,
    });
    while let Some(HeapNode { cost, node }) = heap.pop() {
        if cost > dist[node.index()] {
            continue;
        }
        if node == to {
            break;
        }
        let arrived = prev_edge[node.index()];
        for edge_id in network.adjacency.outgoing(node) {
            let edge = &network.edges[edge_id.index()];
            if let Some(from_edge) = arrived {
                if !turn_allowed(network, node, from_edge, *edge_id) {
                    continue;
                }
            }
            let weight = weights[edge_id.index()];
            if !weight.is_finite() {
                continue;
            }
            let next = edge.to;
            let proposed = cost + weight;
            if proposed < dist[next.index()] {
                dist[next.index()] = proposed;
                prev_edge[next.index()] = Some(*edge_id);
                prev_node[next.index()] = Some(node);
                heap.push(HeapNode {
                    cost: proposed,
                    node: next,
                });
            }
        }
    }
    if !dist[to.index()].is_finite() {
        return None;
    }
    let mut edges = Vec::new();
    let mut node = to;
    for _ in 0..count {
        if node == from {
            break;
        }
        let edge = prev_edge[node.index()]?;
        edges.push(edge);
        node = prev_node[node.index()]?;
    }
    if node != from {
        return None;
    }
    edges.reverse();
    Some((edges, dist[to.index()]))
}

fn turn_allowed(network: &Network, node: NodeId, from: EdgeId, to: EdgeId) -> bool {
    network
        .turns_at(node)
        .iter()
        .any(|turn| turn.from_edge == from && turn.to_edge == to && turn.allowed)
}

struct HeapNode {
    cost: f64,
    node: NodeId,
}

impl PartialEq for HeapNode {
    fn eq(&self, other: &Self) -> bool {
        self.cost.total_cmp(&other.cost) == Ordering::Equal && self.node == other.node
    }
}

impl Eq for HeapNode {}

impl PartialOrd for HeapNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap. Lower cost must compare greater.
        other.cost.total_cmp(&self.cost).then(other.node.cmp(&self.node))
    }
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
}
