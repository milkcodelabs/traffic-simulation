use serde::{Deserialize, Serialize};

use crate::geo::wrap_pi;

/// Right-hand-traffic movement, classified from the change in navigation bearing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Movement {
    Right,
    Through,
    Left,
    UTurn,
}

/// Bearings at the node: arrival is the incoming edge's last segment,
/// departure is the outgoing edge's first segment.
#[derive(Clone, Copy, Debug)]
pub struct Arrival {
    pub incoming_bearing_rad: f64,
    pub outgoing_bearing_rad: f64,
}

pub fn classify_movement(turn: Arrival) -> (Movement, f64) {
    let delta = wrap_pi(turn.outgoing_bearing_rad - turn.incoming_bearing_rad);
    let abs = delta.abs();
    let movement = if abs <= 35.0_f64.to_radians() {
        Movement::Through
    } else if abs >= 150.0_f64.to_radians() {
        Movement::UTurn
    } else if delta > 0.0 {
        Movement::Right
    } else {
        Movement::Left
    };
    (movement, delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eastbound_to_northbound_is_a_left_turn() {
        let (movement, delta) = classify_movement(Arrival {
            incoming_bearing_rad: 90.0_f64.to_radians(),
            outgoing_bearing_rad: 0.0,
        });
        assert_eq!(movement, Movement::Left);
        assert!(delta < 0.0);
    }

    #[test]
    fn eastbound_to_southbound_is_a_right_turn() {
        let (movement, _) = classify_movement(Arrival {
            incoming_bearing_rad: 90.0_f64.to_radians(),
            outgoing_bearing_rad: 180.0_f64.to_radians(),
        });
        assert_eq!(movement, Movement::Right);
    }
}
