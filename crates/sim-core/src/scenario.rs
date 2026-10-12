use serde::{Deserialize, Serialize};

use crate::demand::DEFAULT_ROUTE_INERTIA;
use crate::id::{EdgeId, ZoneId};
use crate::signal::SignalMode;
use crate::time::TimeWindow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SignalSchedule {
    Unchanged,
    ForceFixed,
    ForceFlashingAmber,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeScale {
    pub edge: EdgeId,
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneScale {
    pub zone: ZoneId,
    pub scale: f64,
}

/// Intervention applied on top of a compiled network. Geometry is not copied or edited.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scenario {
    pub name: String,
    pub signal_schedule: SignalSchedule,
    pub night_flashing: Option<TimeWindow>,
    pub edge_capacity_scale: Vec<EdgeScale>,
    pub edge_speed_scale: Vec<EdgeScale>,
    pub zone_demand_scale: Vec<ZoneScale>,
    /// Multiplies every edge speed after the per-edge scales. `1` leaves posted speeds alone.
    pub global_speed_scale: f64,
    /// Multiplies the boundary departure rate. `1` leaves the requested veh/h alone.
    /// This is not an origin-destination matrix.
    pub global_demand_scale: f64,
    /// Bounded-rationality threshold. A path is replaced only when experienced time
    /// exceeds `route_inertia * baseline_travel_time`.
    pub route_inertia: f64,
}

impl Scenario {
    pub fn baseline(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            signal_schedule: SignalSchedule::Unchanged,
            night_flashing: None,
            edge_capacity_scale: Vec::new(),
            edge_speed_scale: Vec::new(),
            zone_demand_scale: Vec::new(),
            global_speed_scale: 1.0,
            global_demand_scale: 1.0,
            route_inertia: DEFAULT_ROUTE_INERTIA,
        }
    }

    pub fn forced_mode(&self, scheduled: SignalMode) -> SignalMode {
        match self.signal_schedule {
            SignalSchedule::Unchanged => scheduled,
            SignalSchedule::ForceFixed => SignalMode::FixedCycle,
            SignalSchedule::ForceFlashingAmber => SignalMode::FlashingAmber,
        }
    }
}

pub fn scaled_capacity(base_veh_per_s: f64, edge: EdgeId, scenario: &Scenario) -> f64 {
    apply_scales(base_veh_per_s, edge, &scenario.edge_capacity_scale)
}

pub fn scaled_speed(base_mps: f64, edge: EdgeId, scenario: &Scenario) -> f64 {
    (apply_scales(base_mps, edge, &scenario.edge_speed_scale)
        * scenario.global_speed_scale.max(0.0))
    .max(0.0)
}

/// Dense capacity and speed multipliers, one per edge. Built once per step so a corridor
/// closure does not rescan the scale list for every edge.
pub fn scale_factors(scenario: &Scenario, edges: usize) -> (Vec<f64>, Vec<f64>) {
    let mut capacity = vec![1.0; edges];
    let mut speed = vec![scenario.global_speed_scale.max(0.0); edges];
    for scale in &scenario.edge_capacity_scale {
        let index = scale.edge.index();
        if index < edges {
            capacity[index] *= scale.scale;
        }
    }
    for scale in &scenario.edge_speed_scale {
        let index = scale.edge.index();
        if index < edges {
            speed[index] *= scale.scale;
        }
    }
    for factor in &mut capacity {
        *factor = factor.max(0.0);
    }
    for factor in &mut speed {
        *factor = factor.max(0.0);
    }
    (capacity, speed)
}

fn apply_scales(base: f64, edge: EdgeId, scales: &[EdgeScale]) -> f64 {
    let factor = scales
        .iter()
        .filter(|scale| scale.edge == edge)
        .map(|scale| scale.scale)
        .fold(1.0, |acc, scale| acc * scale);
    (base * factor).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_scales_stack_and_a_zero_scale_closes_the_link() {
        let scenario = Scenario {
            edge_capacity_scale: vec![
                EdgeScale {
                    edge: EdgeId(3),
                    scale: 0.5,
                },
                EdgeScale {
                    edge: EdgeId(3),
                    scale: 0.5,
                },
            ],
            ..Scenario::baseline("closure")
        };
        assert!((scaled_capacity(2.0, EdgeId(3), &scenario) - 0.5).abs() < 1e-12);
        let closed = Scenario {
            edge_capacity_scale: vec![EdgeScale {
                edge: EdgeId(3),
                scale: 0.0,
            }],
            ..Scenario::baseline("closed")
        };
        assert_eq!(scaled_capacity(2.0, EdgeId(3), &closed), 0.0);
        assert!((scaled_speed(10.0, EdgeId(3), &closed) - 10.0).abs() < 1e-12);
        let slowed = Scenario {
            global_speed_scale: 0.5,
            ..Scenario::baseline("slow")
        };
        assert!((scaled_speed(10.0, EdgeId(1), &slowed) - 5.0).abs() < 1e-12);
        let (capacity, speed) = scale_factors(&scenario, 4);
        assert!((capacity[3] - 0.25).abs() < 1e-12);
        assert!((speed[0] - 1.0).abs() < 1e-12);
    }
}
