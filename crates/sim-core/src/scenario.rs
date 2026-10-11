use serde::{Deserialize, Serialize};

use crate::demand::DEFAULT_ROUTE_INERTIA;
use crate::id::{EdgeId, ZoneId};
use crate::signal::SignalMode;
use crate::time::TimeWindow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    apply_scales(base_mps, edge, &scenario.edge_speed_scale)
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
    }
}
