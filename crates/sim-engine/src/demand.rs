//! Boundary departures at compiled gateway nodes.
//!
//! This is a rate the user sets in the desktop shell. It is not an origin-destination
//! matrix and it is not a calibrated demand. A rate of zero emits nothing.

use sim_core::{Network, NodeId, NodeKind, Scenario};

use crate::route::{free_flow_weights, mark_reached};
use crate::Trip;

/// Where the next boundary trip will be placed. The sequence is deterministic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DemandCursor {
    pub next_s: f64,
    pub index: u64,
}

impl Default for DemandCursor {
    fn default() -> Self {
        Self {
            next_s: 0.0,
            index: 0,
        }
    }
}

pub fn gateway_nodes(network: &Network) -> Vec<NodeId> {
    network
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Gateway)
        .map(|(index, _)| NodeId(index as u32))
        .collect()
}

/// Gateways with a road into the city. Exit-only cordon nodes cannot start a trip.
pub fn entry_gateways(network: &Network) -> Vec<NodeId> {
    gateway_nodes(network)
        .into_iter()
        .filter(|node| !network.adjacency.outgoing(*node).is_empty())
        .collect()
}

/// Gateways with a road out of the city. Entry-only cordon nodes are not sinks.
pub fn exit_gateways(network: &Network) -> Vec<NodeId> {
    gateway_nodes(network)
        .into_iter()
        .filter(|node| !network.adjacency.incoming(*node).is_empty())
        .collect()
}

/// Emit trips with departure times in `(cursor.next_s ..= until_s)`.
///
/// A non-positive rate advances the cursor to `until_s` and emits nothing, so turning
/// the rate up later does not release a backlog of imaginary past trips.
pub fn emit_gateway_trips(
    origins: &[NodeId],
    destinations: &[NodeId],
    vehicles_per_hour: f64,
    cursor: &mut DemandCursor,
    until_s: f64,
) -> Vec<Trip> {
    if vehicles_per_hour <= 0.0
        || !vehicles_per_hour.is_finite()
        || origins.is_empty()
        || destinations.is_empty()
    {
        cursor.next_s = until_s.max(cursor.next_s);
        return Vec::new();
    }
    let interval = 3600.0 / vehicles_per_hour;
    if interval <= 0.0 || !interval.is_finite() {
        cursor.next_s = until_s.max(cursor.next_s);
        return Vec::new();
    }
    let mut trips = Vec::new();
    let origins_len = origins.len();
    let destinations_len = destinations.len();
    while cursor.next_s <= until_s + 1e-9 && trips.len() < 100_000 {
        let slot = cursor.index as usize;
        let origin = origins[slot % origins_len];
        let mut destination_slot = slot.wrapping_mul(7).wrapping_add(1) % destinations_len;
        if destinations[destination_slot] == origin {
            destination_slot = (destination_slot + 1) % destinations_len;
        }
        trips.push(Trip::new(
            cursor.next_s,
            origin,
            destinations[destination_slot],
        ));
        cursor.index += 1;
        cursor.next_s += interval;
    }
    trips
}

/// Cursor for a seeded boundary process. `credit` carries a fractional zone-scale emission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeededCursor {
    pub next_s: f64,
    pub index: u64,
    pub credit: f64,
}

impl Default for SeededCursor {
    fn default() -> Self {
        Self {
            next_s: 0.0,
            index: 0,
            credit: 0.0,
        }
    }
}

/// Zone demand multiplier for each gateway, in the same order as `gateways`.
/// A missing zone scale is 1. This does not invent an origin-destination table.
pub fn gateway_zone_scales(
    network: &Network,
    scenario: &Scenario,
    gateways: &[NodeId],
) -> Vec<f64> {
    let mut node_scale = vec![1.0_f64; network.nodes.len()];
    for scale in &scenario.zone_demand_scale {
        let Some(zone) = network.zones.get(scale.zone.index()) else {
            continue;
        };
        let factor = scale.scale.max(0.0);
        for node in &zone.nodes {
            if node.index() < node_scale.len() {
                node_scale[node.index()] *= factor;
            }
        }
    }
    gateways
        .iter()
        .map(|node| node_scale[node.index()])
        .collect()
}

/// Seeded boundary trips. The same seed repeats the same departures.
/// Destination pairing and headway jitter come from the seed. Zone scales add or
/// withhold trips at an origin without changing the headway clock.
pub fn emit_seeded_gateway_trips(
    origins: &[NodeId],
    destinations: &[NodeId],
    vehicles_per_hour: f64,
    seed: u64,
    zone_scale: &[f64],
    cursor: &mut SeededCursor,
    until_s: f64,
) -> Vec<Trip> {
    if vehicles_per_hour <= 0.0
        || !vehicles_per_hour.is_finite()
        || origins.is_empty()
        || destinations.is_empty()
    {
        cursor.next_s = until_s.max(cursor.next_s);
        return Vec::new();
    }
    let interval = 3600.0 / vehicles_per_hour;
    if interval <= 0.0 || !interval.is_finite() {
        cursor.next_s = until_s.max(cursor.next_s);
        return Vec::new();
    }
    let origins_len = origins.len();
    let destinations_len = destinations.len();
    let mut trips = Vec::new();
    while cursor.next_s <= until_s + 1e-9 && trips.len() < 100_000 {
        let slot = cursor.index as usize;
        let origin_slot = slot % origins_len;
        let scale = zone_scale.get(origin_slot).copied().unwrap_or(1.0).max(0.0);
        cursor.credit += scale;
        let mut emitted = 0;
        while cursor.credit >= 1.0 && emitted < 8 && trips.len() < 100_000 {
            cursor.credit -= 1.0;
            let origin = origins[origin_slot];
            let mut destination_slot = mix(seed, cursor.index) as usize % destinations_len;
            if destinations[destination_slot] == origin {
                destination_slot = (destination_slot + 1) % destinations_len;
            }
            let destination = destinations[destination_slot];
            if destination != origin {
                trips.push(Trip::new(cursor.next_s, origin, destination));
            }
            emitted += 1;
        }
        cursor.index += 1;
        let unit = unit_interval(seed, cursor.index);
        cursor.next_s += interval * (0.75 + 0.5 * unit);
    }
    trips
}

fn mix(seed: u64, index: u64) -> u64 {
    splitmix(seed ^ index.wrapping_mul(0x9E3779B97F4A7C15))
}

fn unit_interval(seed: u64, index: u64) -> f64 {
    let bits = splitmix(seed.wrapping_add(index.wrapping_mul(0xD1B54A32D192ED03)));
    (bits >> 11) as f64 / ((1u64 << 53) as f64)
}

fn splitmix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E3779B97F4A7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D049BB133111EB);
    value ^ (value >> 31)
}

/// Entry and exit gateways that sit in the giant connected component and that the
/// turn-aware router can actually join. A trip outside this set is not emitted.
#[derive(Clone, Debug)]
pub struct BoundaryDemand {
    pub origins: Vec<NodeId>,
    pub destinations: Vec<NodeId>,
    pub giant_nodes: usize,
    pub dropped_entries: usize,
    pub dropped_exits: usize,
    reached: Vec<Vec<u64>>,
    origin_slot: Vec<u32>,
}

impl BoundaryDemand {
    pub fn allows(&self, origin: NodeId, destination: NodeId) -> bool {
        let Some(slot) = self.origin_slot.get(origin.index()).copied() else {
            return false;
        };
        if slot == u32::MAX || destination.index() / 64 >= self.reached[slot as usize].len() {
            return false;
        }
        let word = destination.index() / 64;
        let mask = 1_u64 << (destination.index() % 64);
        self.reached[slot as usize][word] & mask != 0
    }
}

pub fn boundary_demand(network: &Network, scenario: &Scenario) -> BoundaryDemand {
    let giant = giant_component(network);
    let giant_nodes = giant.iter().filter(|inside| **inside).count();
    let entries = entry_gateways(network);
    let exits = exit_gateways(network);
    let words = network.nodes.len().div_ceil(64);
    let weights = free_flow_weights(network, scenario);
    let mut origins = Vec::new();
    let mut reached = Vec::new();
    let mut bits = vec![0_u64; words];
    let mut dropped_entries = 0_usize;
    for origin in &entries {
        if !giant[origin.index()] {
            dropped_entries += 1;
            continue;
        }
        bits.fill(0);
        mark_reached(network, &weights, *origin, &mut bits);
        let can_leave = exits
            .iter()
            .any(|exit| *exit != *origin && giant[exit.index()] && bit_set(&bits, exit.index()));
        if !can_leave {
            dropped_entries += 1;
            continue;
        }
        origins.push(*origin);
        reached.push(bits.clone());
    }
    let mut destinations = Vec::new();
    let mut dropped_exits = 0_usize;
    for exit in &exits {
        if !giant[exit.index()] {
            dropped_exits += 1;
            continue;
        }
        let reached_by_some = origins
            .iter()
            .enumerate()
            .any(|(slot, origin)| *origin != *exit && bit_set(&reached[slot], exit.index()));
        if reached_by_some {
            destinations.push(*exit);
        } else {
            dropped_exits += 1;
        }
    }
    let mut origin_slot = vec![u32::MAX; network.nodes.len()];
    for (slot, origin) in origins.iter().enumerate() {
        origin_slot[origin.index()] = slot as u32;
    }
    eprintln!(
        "giant component {giant_nodes} nodes  routable entries {}  routable exits {}  dropped entries {dropped_entries}  dropped exits {dropped_exits}",
        origins.len(),
        destinations.len()
    );
    BoundaryDemand {
        origins,
        destinations,
        giant_nodes,
        dropped_entries,
        dropped_exits,
        reached,
        origin_slot,
    }
}

/// Drop trips the router cannot path. The headway clock has already consumed the slot.
pub fn retain_routable(
    trips: Vec<Trip>,
    boundary: &BoundaryDemand,
    rejected: &mut u64,
) -> Vec<Trip> {
    trips
        .into_iter()
        .filter(|trip| {
            if boundary.allows(trip.origin, trip.destination) {
                true
            } else {
                *rejected += 1;
                #[cfg(debug_assertions)]
                if *rejected <= 12 || (*rejected).is_multiple_of(1000) {
                    eprintln!(
                        "rejected unroutable trip {:.0}s {} -> {} ({} so far)",
                        trip.depart_s, trip.origin.0, trip.destination.0, *rejected
                    );
                }
                false
            }
        })
        .collect()
}

fn bit_set(bits: &[u64], index: usize) -> bool {
    let word = index / 64;
    word < bits.len() && bits[word] & (1_u64 << (index % 64)) != 0
}

fn giant_component(network: &Network) -> Vec<bool> {
    let count = network.nodes.len();
    let mut parent: Vec<u32> = (0..count as u32).collect();
    let mut rank = vec![0_u8; count];
    for edge in &network.edges {
        union(&mut parent, &mut rank, edge.from.0, edge.to.0);
    }
    let mut size = vec![0_u32; count];
    for index in 0..count {
        let root = find(&mut parent, index as u32) as usize;
        size[root] += 1;
    }
    let root = size
        .iter()
        .enumerate()
        .max_by_key(|(_, nodes)| *nodes)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let mut inside = vec![false; count];
    for (index, member) in inside.iter_mut().enumerate() {
        if find(&mut parent, index as u32) as usize == root {
            *member = true;
        }
    }
    inside
}

fn find(parent: &mut [u32], mut index: u32) -> u32 {
    while parent[index as usize] != index {
        parent[index as usize] = parent[parent[index as usize] as usize];
        index = parent[index as usize];
    }
    index
}

fn union(parent: &mut [u32], rank: &mut [u8], left: u32, right: u32) {
    let mut left = find(parent, left);
    let mut right = find(parent, right);
    if left == right {
        return;
    }
    if rank[left as usize] < rank[right as usize] {
        std::mem::swap(&mut left, &mut right);
    }
    parent[right as usize] = left;
    if rank[left as usize] == rank[right as usize] {
        rank[left as usize] = rank[left as usize].saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_rate_does_not_backfill_when_the_rate_turns_on() {
        let gates = [NodeId(0), NodeId(1), NodeId(2)];
        let mut cursor = DemandCursor::default();
        let quiet = emit_gateway_trips(&gates, &gates, 0.0, &mut cursor, 100.0);
        assert!(quiet.is_empty());
        assert!((cursor.next_s - 100.0).abs() < 1e-9);
        let started = emit_gateway_trips(&gates, &gates, 3600.0, &mut cursor, 102.0);
        assert_eq!(started.len(), 3);
        assert!(started.iter().all(|trip| trip.depart_s >= 100.0));
        assert!(started.iter().all(|trip| trip.origin != trip.destination));
    }

    #[test]
    fn one_vehicle_per_second_is_a_fixed_gateway_cycle() {
        let gates = [NodeId(4), NodeId(8), NodeId(9)];
        let mut cursor = DemandCursor::default();
        let trips = emit_gateway_trips(&gates, &gates, 3600.0, &mut cursor, 2.0);
        let pairs: Vec<_> = trips
            .iter()
            .map(|trip| (trip.depart_s, trip.origin, trip.destination))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (0.0, NodeId(4), NodeId(8)),
                (1.0, NodeId(8), NodeId(9)),
                (2.0, NodeId(9), NodeId(4)),
            ]
        );
    }

    #[test]
    fn a_seed_repeats_and_a_zero_zone_scale_emits_nothing() {
        let gates = [NodeId(1), NodeId(2), NodeId(3)];
        let mut first = SeededCursor::default();
        let mut second = SeededCursor::default();
        let left = emit_seeded_gateway_trips(
            &gates,
            &gates,
            3600.0,
            11,
            &[1.0, 1.0, 1.0],
            &mut first,
            4.0,
        );
        let right = emit_seeded_gateway_trips(
            &gates,
            &gates,
            3600.0,
            11,
            &[1.0, 1.0, 1.0],
            &mut second,
            4.0,
        );
        assert_eq!(left.len(), right.len());
        assert!(left.len() >= 3);
        assert!(left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.origin == b.origin && a.destination == b.destination));
        let mut other = SeededCursor::default();
        let changed = emit_seeded_gateway_trips(
            &gates,
            &gates,
            3600.0,
            99,
            &[1.0, 1.0, 1.0],
            &mut other,
            4.0,
        );
        let identical = left.len() == changed.len()
            && left.iter().zip(changed.iter()).all(|(a, b)| {
                a.destination == b.destination && (a.depart_s - b.depart_s).abs() < 1e-9
            });
        assert!(!identical);
        let mut quiet = SeededCursor::default();
        let none = emit_seeded_gateway_trips(
            &gates,
            &gates,
            3600.0,
            11,
            &[0.0, 0.0, 0.0],
            &mut quiet,
            4.0,
        );
        assert!(none.is_empty());
        assert!(quiet.next_s > 4.0);
    }
}
