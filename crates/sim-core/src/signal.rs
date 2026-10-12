use serde::{Deserialize, Serialize};

use crate::id::{EdgeId, NodeId, SignalId};
use crate::time::TimeWindow;

/// Gap-acceptance delay imposed on the minor movement under flashing amber or an unsignalized yield.
pub const TAU_YIELD_S: f64 = 4.5;
const YELLOW_S: f64 = 3.0;
const ALL_RED_S: f64 = 1.5;
const MIN_GREEN_S: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalMode {
    FixedCycle,
    FlashingAmber,
    Off,
}

impl SignalMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::FixedCycle => "fixed-cycle",
            Self::FlashingAmber => "flashing-amber",
            Self::Off => "off",
        }
    }
}

pub fn mode_label(mode: SignalMode) -> &'static str {
    mode.name()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Phase {
    pub green_s: f64,
    pub yellow_s: f64,
    pub all_red_s: f64,
    pub incoming: Vec<EdgeId>,
}

/// One incoming approach at a signalized node.
#[derive(Clone, Copy, Debug)]
pub struct Approach {
    pub edge: EdgeId,
    pub bearing_rad: f64,
    pub lanes: u8,
    pub rank: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalController {
    pub id: SignalId,
    pub node: NodeId,
    pub mode_outside_night: SignalMode,
    pub night_window: TimeWindow,
    pub night_mode: SignalMode,
    pub tau_yield_s: f64,
    pub cycle_length_s: f64,
    pub offset_s: f64,
    pub phases: Vec<Phase>,
}

impl SignalController {
    pub fn mode_at(&self, seconds_from_midnight: u32) -> SignalMode {
        if self.night_window.contains(seconds_from_midnight) {
            self.night_mode
        } else {
            self.mode_outside_night
        }
    }
}

/// Build a fixed cycle from the approaches that exist.
///
/// Opposing quadrants share a phase when one axis strictly outranks the other.
/// Equal-rank axes are split into one phase per occupied quadrant so conflicting
/// movements are not given green together without a priority distinction.
pub fn plan_signal(node: NodeId, id: SignalId, approaches: &[Approach]) -> SignalController {
    let groups = phase_groups(approaches);
    let major = approaches
        .iter()
        .any(|approach| approach.rank >= crate::road::PRIMARY_RANK);
    let target = if major { 90.0 } else { 60.0 };
    let greens = allocate_greens(&groups, approaches, target);
    let phases = groups
        .into_iter()
        .zip(greens.iter().copied())
        .map(|(group, green_s)| Phase {
            green_s,
            yellow_s: YELLOW_S,
            all_red_s: ALL_RED_S,
            incoming: group
                .into_iter()
                .map(|index| approaches[index].edge)
                .collect(),
        })
        .collect::<Vec<_>>();
    let cycle_length_s = phases
        .iter()
        .map(|phase| phase.green_s + phase.yellow_s + phase.all_red_s)
        .sum();
    SignalController {
        id,
        node,
        mode_outside_night: SignalMode::FixedCycle,
        night_window: TimeWindow::NIGHT_FLASHING,
        night_mode: SignalMode::FlashingAmber,
        tau_yield_s: TAU_YIELD_S,
        cycle_length_s,
        offset_s: 0.0,
        phases,
    }
}

fn quadrant(bearing_rad: f64) -> usize {
    let degrees = bearing_rad.to_degrees().rem_euclid(360.0);
    ((degrees / 90.0).floor() as usize) % 4
}

fn phase_groups(approaches: &[Approach]) -> Vec<Vec<usize>> {
    if approaches.is_empty() {
        return Vec::new();
    }
    let mut buckets: [Vec<usize>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for (index, approach) in approaches.iter().enumerate() {
        buckets[quadrant(approach.bearing_rad)].push(index);
    }
    let rank = |quadrants: &[usize]| -> Option<u8> {
        quadrants
            .iter()
            .flat_map(|quadrant| {
                buckets[*quadrant]
                    .iter()
                    .map(|index| approaches[*index].rank)
            })
            .max()
    };
    let ns = rank(&[0, 2]);
    let ew = rank(&[1, 3]);
    match (ns, ew) {
        (Some(ns_rank), Some(ew_rank)) if ns_rank != ew_rank => {
            let ns_group = [buckets[0].clone(), buckets[2].clone()].concat();
            let ew_group = [buckets[1].clone(), buckets[3].clone()].concat();
            if ns_rank > ew_rank {
                vec![ns_group, ew_group]
            } else {
                vec![ew_group, ns_group]
            }
        }
        (Some(_), Some(_)) => (0..4)
            .filter(|quadrant| !buckets[*quadrant].is_empty())
            .map(|quadrant| buckets[quadrant].clone())
            .collect(),
        _ => vec![(0..approaches.len()).collect()],
    }
}

fn allocate_greens(
    groups: &[Vec<usize>],
    approaches: &[Approach],
    target_cycle_s: f64,
) -> Vec<f64> {
    if groups.is_empty() {
        return Vec::new();
    }
    let count = groups.len();
    let lost = count as f64 * (YELLOW_S + ALL_RED_S);
    let budget = (target_cycle_s - lost).max(count as f64 * MIN_GREEN_S);
    let lanes: Vec<f64> = groups
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|index| f64::from(approaches[*index].lanes))
                .sum::<f64>()
                .max(1.0)
        })
        .collect();
    let lane_total: f64 = lanes.iter().sum();
    let mut greens: Vec<f64> = lanes
        .iter()
        .map(|lanes| budget * lanes / lane_total)
        .collect();
    let mut pinned = vec![false; greens.len()];
    for _ in 0..greens.len() {
        let mut deficit = 0.0;
        for (index, green) in greens.iter_mut().enumerate() {
            if *green < MIN_GREEN_S {
                deficit += MIN_GREEN_S - *green;
                *green = MIN_GREEN_S;
                pinned[index] = true;
            }
        }
        if deficit == 0.0 {
            break;
        }
        let free_lanes: f64 = lanes
            .iter()
            .enumerate()
            .filter(|(index, _)| !pinned[*index])
            .map(|(_, lanes)| *lanes)
            .sum();
        if free_lanes <= 0.0 {
            break;
        }
        for (index, green) in greens.iter_mut().enumerate() {
            if !pinned[index] {
                *green -= deficit * lanes[index] / free_lanes;
            }
        }
    }
    greens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approach(index: u32, bearing_deg: f64, lanes: u8, rank: u8) -> Approach {
        Approach {
            edge: EdgeId(index),
            bearing_rad: bearing_deg.to_radians(),
            lanes,
            rank,
        }
    }

    #[test]
    fn major_road_receives_the_larger_green_on_a_ninety_second_cycle() {
        let approaches = [
            approach(0, 0.0, 3, 6),
            approach(1, 180.0, 3, 6),
            approach(2, 90.0, 1, 4),
            approach(3, 270.0, 1, 4),
        ];
        let plan = plan_signal(NodeId(0), SignalId(0), &approaches);
        assert_eq!(plan.phases.len(), 2);
        assert!((plan.cycle_length_s - 90.0).abs() < 1e-9);
        assert!(plan.phases[0].green_s > plan.phases[1].green_s);
        assert_eq!(plan.mode_at(3 * 3600), SignalMode::FlashingAmber);
        assert_eq!(plan.mode_at(12 * 3600), SignalMode::FixedCycle);
        assert!((plan.tau_yield_s - TAU_YIELD_S).abs() < 1e-12);
    }

    #[test]
    fn equal_rank_approaches_do_not_share_a_phase() {
        let approaches = [
            approach(0, 10.0, 1, 4),
            approach(1, 100.0, 1, 4),
            approach(2, 190.0, 1, 4),
            approach(3, 280.0, 1, 4),
        ];
        let plan = plan_signal(NodeId(1), SignalId(0), &approaches);
        assert_eq!(plan.phases.len(), 4);
        assert!(plan.phases.iter().all(|phase| phase.incoming.len() == 1));
    }
}
