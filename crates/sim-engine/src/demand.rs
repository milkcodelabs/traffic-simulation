//! Boundary departures at compiled gateway nodes.
//!
//! This is a rate the user sets in the desktop shell. It is not an origin-destination
//! matrix and it is not a calibrated demand. A rate of zero emits nothing.

use sim_core::{Network, NodeId, NodeKind};

use crate::Trip;

/// Where the next boundary trip will be placed. The sequence is deterministic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DemandCursor {
    pub next_s: f64,
    pub index: u64,
}

impl Default for DemandCursor {
    fn default() -> Self {
        Self { next_s: 0.0, index: 0 }
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

/// Emit trips with departure times in `(cursor.next_s ..= until_s)`.
///
/// A non-positive rate advances the cursor to `until_s` and emits nothing, so turning
/// the rate up later does not release a backlog of imaginary past trips.
pub fn emit_gateway_trips(gateways: &[NodeId], vehicles_per_hour: f64, cursor: &mut DemandCursor, until_s: f64) -> Vec<Trip> {
    if !(vehicles_per_hour > 0.0) || !vehicles_per_hour.is_finite() || gateways.len() < 2 {
        cursor.next_s = until_s.max(cursor.next_s);
        return Vec::new();
    }
    let interval = 3600.0 / vehicles_per_hour;
    if !(interval > 0.0) || !interval.is_finite() {
        cursor.next_s = until_s.max(cursor.next_s);
        return Vec::new();
    }
    let mut trips = Vec::new();
    let count = gateways.len();
    while cursor.next_s <= until_s + 1e-9 && trips.len() < 100_000 {
        let slot = cursor.index as usize;
        let origin = gateways[slot % count];
        let mut destination_slot = slot.wrapping_mul(7).wrapping_add(1) % count;
        if gateways[destination_slot] == origin {
            destination_slot = (destination_slot + 1) % count;
        }
        trips.push(Trip::new(cursor.next_s, origin, gateways[destination_slot]));
        cursor.index += 1;
        cursor.next_s += interval;
    }
    trips
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_rate_does_not_backfill_when_the_rate_turns_on() {
        let gates = [NodeId(0), NodeId(1), NodeId(2)];
        let mut cursor = DemandCursor::default();
        let quiet = emit_gateway_trips(&gates, 0.0, &mut cursor, 100.0);
        assert!(quiet.is_empty());
        assert!((cursor.next_s - 100.0).abs() < 1e-9);
        let started = emit_gateway_trips(&gates, 3600.0, &mut cursor, 102.0);
        assert_eq!(started.len(), 3);
        assert!(started.iter().all(|trip| trip.depart_s >= 100.0));
        assert!(started.iter().all(|trip| trip.origin != trip.destination));
    }

    #[test]
    fn one_vehicle_per_second_is_a_fixed_gateway_cycle() {
        let gates = [NodeId(4), NodeId(8), NodeId(9)];
        let mut cursor = DemandCursor::default();
        let trips = emit_gateway_trips(&gates, 3600.0, &mut cursor, 2.0);
        let pairs: Vec<_> = trips.iter().map(|trip| (trip.depart_s, trip.origin, trip.destination)).collect();
        assert_eq!(
            pairs,
            vec![
                (0.0, NodeId(4), NodeId(8)),
                (1.0, NodeId(8), NodeId(9)),
                (2.0, NodeId(9), NodeId(4)),
            ]
        );
    }
}
