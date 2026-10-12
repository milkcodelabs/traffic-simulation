//! Corridor edits compiled into scenario scales. The graph geometry is not touched.

use sim_core::{Edge, EdgeId, EdgeScale, Network, RoadClass};

/// Share of lanes still open. Closing every lane yields 0, which stops discharge.
pub fn closure_scale(lanes: u8, lanes_closed: u8) -> f64 {
    let lanes = u32::from(lanes.max(1));
    let open = lanes.saturating_sub(u32::from(lanes_closed));
    f64::from(open) / f64::from(lanes)
}

pub fn text_is_ring(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("ring") || lower.contains("περιφερ")
}

/// Motorway, trunk, and roads whose name or ref reads as a ring.
/// Thessaloniki's ring is tagged in that set; the match does not hardcode a way id.
pub fn is_ring_or_motorway(edge: &Edge) -> bool {
    matches!(
        edge.class,
        RoadClass::Motorway | RoadClass::MotorwayLink | RoadClass::Trunk | RoadClass::TrunkLink
    ) || edge.name.as_deref().is_some_and(text_is_ring)
        || edge.road_ref.as_deref().is_some_and(text_is_ring)
}

pub fn lane_closure_scales(network: &Network, lanes_closed: u8) -> Vec<EdgeScale> {
    if lanes_closed == 0 {
        return Vec::new();
    }
    network
        .edges
        .iter()
        .enumerate()
        .filter_map(|(index, edge)| {
            if !is_ring_or_motorway(edge) {
                return None;
            }
            Some(EdgeScale {
                edge: EdgeId(index as u32),
                scale: closure_scale(edge.lanes, lanes_closed),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_one_of_two_lanes_halves_capacity_and_a_ring_name_matches() {
        assert!((closure_scale(2, 1) - 0.5).abs() < 1e-12);
        assert_eq!(closure_scale(1, 2), 0.0);
        assert_eq!(closure_scale(3, 0), 1.0);
        assert!(text_is_ring("Εσωτερική Περιφερειακή"));
        assert!(text_is_ring("Thessaloniki Ring Road"));
        assert!(!text_is_ring("Tsimiski"));
    }
}
