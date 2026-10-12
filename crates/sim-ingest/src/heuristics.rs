use sim_core::{default_lanes, default_speed_mps, parse_highway, RoadClass};

use crate::model::OsmWay;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Travel {
    Forward,
    Backward,
}

#[derive(Clone, Debug)]
pub struct ResolvedWay {
    pub class: RoadClass,
    pub destination_only: bool,
    pub roundabout: bool,
    pub bridge: bool,
    pub tunnel: bool,
    pub directions: Vec<Travel>,
    pub speed_mps: f64,
    pub speed_warning: bool,
}

pub fn resolve_way(way: &OsmWay) -> Option<ResolvedWay> {
    if way.area || way.nodes.len() < 2 {
        return None;
    }
    if matches!(way.highway.as_str(), "proposed" | "construction") {
        return None;
    }
    if blocked_access(way.access.as_deref())
        || blocked_access(way.motor_vehicle.as_deref())
        || blocked_access(way.motorcar.as_deref())
    {
        return None;
    }
    if way.service.as_deref().is_some_and(blocked_service) {
        return None;
    }
    let class = parse_highway(&way.highway)?;
    let roundabout = matches!(way.junction.as_deref(), Some("roundabout" | "circular"));
    let (speed_mps, speed_warning) = resolve_speed(way.maxspeed.as_deref(), class);
    Some(ResolvedWay {
        class,
        destination_only: way
            .access
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("destination")),
        roundabout,
        bridge: way.bridge,
        tunnel: way.tunnel,
        directions: directions(way.oneway.as_deref(), class, roundabout),
        speed_mps,
        speed_warning,
    })
}

pub fn lanes_for(way: &OsmWay, class: RoadClass, travel: Travel, oneway: bool) -> u8 {
    let along_way = travel == Travel::Forward;
    if along_way {
        if let Some(lanes) = way.lanes_forward.as_deref().and_then(parse_lane_count) {
            return lanes;
        }
    } else if let Some(lanes) = way.lanes_backward.as_deref().and_then(parse_lane_count) {
        return lanes;
    }
    if let Some(total) = way.lanes.as_deref().and_then(parse_lane_count) {
        if oneway {
            return total;
        }
        if total == 1 {
            return 1;
        }
        let half = total / 2;
        let extra = total % 2;
        return if along_way { half + extra } else { half.max(1) };
    }
    default_lanes(class, oneway)
}

fn directions(oneway: Option<&str>, class: RoadClass, roundabout: bool) -> Vec<Travel> {
    let implicit = matches!(class, RoadClass::Motorway | RoadClass::MotorwayLink) || roundabout;
    match oneway.map(|value| value.to_ascii_lowercase()).as_deref() {
        Some("yes" | "true" | "1") => vec![Travel::Forward],
        Some("-1" | "reverse") => vec![Travel::Backward],
        Some("no" | "false" | "0") => vec![Travel::Forward, Travel::Backward],
        _ if implicit => vec![Travel::Forward],
        _ => vec![Travel::Forward, Travel::Backward],
    }
}

fn resolve_speed(raw: Option<&str>, class: RoadClass) -> (f64, bool) {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return (default_speed_mps(class), false);
    };
    let lower = raw.to_ascii_lowercase();
    if matches!(lower.as_str(), "signals" | "variable") {
        return (default_speed_mps(class), false);
    }
    if lower == "none" {
        let speed = if matches!(class, RoadClass::Motorway | RoadClass::MotorwayLink) {
            130.0 / 3.6
        } else {
            default_speed_mps(class)
        };
        return (speed, false);
    }
    if let Some(kph) = match lower.as_str() {
        "gr:urban" => Some(50.0),
        "gr:rural" => Some(90.0),
        "gr:trunk" => Some(110.0),
        "gr:motorway" => Some(130.0),
        _ => None,
    } {
        return (kph / 3.6, false);
    }
    match parse_speed_kph(&lower) {
        Some(kph) if kph > 0.0 && kph < 400.0 => (kph / 3.6, false),
        _ => (default_speed_mps(class), true),
    }
}

fn parse_speed_kph(value: &str) -> Option<f64> {
    let mph = value.contains("mph");
    let mut number = String::new();
    for ch in value.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            number.push(ch);
        } else if number.is_empty() {
            continue;
        } else {
            break;
        }
    }
    let parsed: f64 = number.parse().ok()?;
    Some(if mph { parsed * 1.609344 } else { parsed })
}

fn parse_lane_count(value: &str) -> Option<u8> {
    let count: u32 = value.trim().parse().ok()?;
    if (1..=16).contains(&count) {
        Some(count as u8)
    } else {
        None
    }
}

fn blocked_access(value: Option<&str>) -> bool {
    matches!(
        value.map(|item| item.to_ascii_lowercase()).as_deref(),
        Some("no" | "private")
    )
}

fn blocked_service(value: &str) -> bool {
    matches!(
        value,
        "parking_aisle" | "parking" | "driveway" | "emergency_access" | "slipway"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn way(highway: &str) -> OsmWay {
        OsmWay {
            id: 1,
            nodes: vec![1, 2],
            highway: highway.into(),
            oneway: None,
            lanes: None,
            lanes_forward: None,
            lanes_backward: None,
            maxspeed: None,
            name: None,
            road_ref: None,
            junction: None,
            bridge: false,
            tunnel: false,
            access: None,
            motor_vehicle: None,
            motorcar: None,
            service: None,
            area: false,
        }
    }

    #[test]
    fn missing_primary_speed_uses_the_urban_default() {
        let resolved = resolve_way(&way("primary")).unwrap();
        assert!((resolved.speed_mps * 3.6 - 50.0).abs() < 1e-9);
        assert!(!resolved.speed_warning);
    }

    #[test]
    fn greek_motorway_tag_and_miles_are_parsed() {
        let mut motorway = way("motorway");
        motorway.maxspeed = Some("GR:motorway".into());
        let resolved = resolve_way(&motorway).unwrap();
        assert!((resolved.speed_mps * 3.6 - 130.0).abs() < 1e-6);
        assert_eq!(resolved.directions, vec![Travel::Forward]);

        motorway.maxspeed = Some("30 mph".into());
        let resolved = resolve_way(&motorway).unwrap();
        assert!((resolved.speed_mps * 3.6 - 30.0 * 1.609344).abs() < 1e-6);
    }

    #[test]
    fn forward_lane_tag_wins_and_odd_totals_favour_the_way_direction() {
        let mut street = way("secondary");
        street.oneway = Some("no".into());
        street.lanes = Some("3".into());
        street.lanes_forward = Some("2".into());
        street.lanes_backward = Some("1".into());
        let class = RoadClass::Secondary;
        assert_eq!(lanes_for(&street, class, Travel::Forward, false), 2);
        assert_eq!(lanes_for(&street, class, Travel::Backward, false), 1);

        street.lanes_forward = None;
        street.lanes_backward = None;
        assert_eq!(lanes_for(&street, class, Travel::Forward, false), 2);
        assert_eq!(lanes_for(&street, class, Travel::Backward, false), 1);
    }

    #[test]
    fn private_and_parking_aisles_are_dropped() {
        let mut private = way("residential");
        private.access = Some("private".into());
        assert!(resolve_way(&private).is_none());
        let mut aisle = way("service");
        aisle.service = Some("parking_aisle".into());
        assert!(resolve_way(&aisle).is_none());
        let mut destination = way("residential");
        destination.access = Some("destination".into());
        assert!(resolve_way(&destination).unwrap().destination_only);
    }
}
