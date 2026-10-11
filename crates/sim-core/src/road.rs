use serde::{Deserialize, Serialize};

/// Normalized driveable class. Link classes stay distinct so ramp geometry is visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoadClass {
    Motorway,
    MotorwayLink,
    Trunk,
    TrunkLink,
    Primary,
    PrimaryLink,
    Secondary,
    SecondaryLink,
    Tertiary,
    TertiaryLink,
    Unclassified,
    Residential,
    LivingStreet,
    Service,
}

impl RoadClass {
    pub fn is_link(self) -> bool {
        matches!(
            self,
            Self::MotorwayLink | Self::TrunkLink | Self::PrimaryLink | Self::SecondaryLink | Self::TertiaryLink
        )
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Motorway | Self::MotorwayLink => 8,
            Self::Trunk | Self::TrunkLink => 7,
            Self::Primary | Self::PrimaryLink => 6,
            Self::Secondary | Self::SecondaryLink => 5,
            Self::Tertiary | Self::TertiaryLink => 4,
            Self::Unclassified => 3,
            Self::Residential => 2,
            Self::LivingStreet => 1,
            Self::Service => 0,
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Motorway => 0,
            Self::MotorwayLink => 1,
            Self::Trunk => 2,
            Self::TrunkLink => 3,
            Self::Primary => 4,
            Self::PrimaryLink => 5,
            Self::Secondary => 6,
            Self::SecondaryLink => 7,
            Self::Tertiary => 8,
            Self::TertiaryLink => 9,
            Self::Unclassified => 10,
            Self::Residential => 11,
            Self::LivingStreet => 12,
            Self::Service => 13,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Motorway => "motorway",
            Self::MotorwayLink => "motorway_link",
            Self::Trunk => "trunk",
            Self::TrunkLink => "trunk_link",
            Self::Primary => "primary",
            Self::PrimaryLink => "primary_link",
            Self::Secondary => "secondary",
            Self::SecondaryLink => "secondary_link",
            Self::Tertiary => "tertiary",
            Self::TertiaryLink => "tertiary_link",
            Self::Unclassified => "unclassified",
            Self::Residential => "residential",
            Self::LivingStreet => "living_street",
            Self::Service => "service",
        }
    }
}

/// Rank at which an approach counts as a major road for the default signal cycle.
pub const PRIMARY_RANK: u8 = 6;

pub fn parse_highway(value: &str) -> Option<RoadClass> {
    match value {
        "motorway" => Some(RoadClass::Motorway),
        "motorway_link" => Some(RoadClass::MotorwayLink),
        "trunk" => Some(RoadClass::Trunk),
        "trunk_link" => Some(RoadClass::TrunkLink),
        "primary" => Some(RoadClass::Primary),
        "primary_link" => Some(RoadClass::PrimaryLink),
        "secondary" => Some(RoadClass::Secondary),
        "secondary_link" => Some(RoadClass::SecondaryLink),
        "tertiary" => Some(RoadClass::Tertiary),
        "tertiary_link" => Some(RoadClass::TertiaryLink),
        "unclassified" => Some(RoadClass::Unclassified),
        "residential" => Some(RoadClass::Residential),
        "living_street" => Some(RoadClass::LivingStreet),
        "service" => Some(RoadClass::Service),
        _ => None,
    }
}

/// European urban posted-speed fallback, in m/s, used only when OSM has no usable maxspeed.
pub fn default_speed_mps(class: RoadClass) -> f64 {
    let kph = match class {
        RoadClass::Motorway => 130.0,
        RoadClass::MotorwayLink => 70.0,
        RoadClass::Trunk => 90.0,
        RoadClass::TrunkLink => 50.0,
        RoadClass::Primary => 50.0,
        RoadClass::PrimaryLink => 40.0,
        RoadClass::Secondary => 50.0,
        RoadClass::SecondaryLink => 40.0,
        RoadClass::Tertiary => 40.0,
        RoadClass::TertiaryLink => 30.0,
        RoadClass::Unclassified => 40.0,
        RoadClass::Residential => 30.0,
        RoadClass::LivingStreet => 20.0,
        RoadClass::Service => 20.0,
    };
    kph / 3.6
}

/// Lane fallback in the direction of travel.
pub fn default_lanes(class: RoadClass, oneway: bool) -> u8 {
    match class {
        RoadClass::Motorway | RoadClass::Trunk => 2,
        RoadClass::Primary if oneway => 2,
        _ => 1,
    }
}

pub fn jam_spacing_m(class: RoadClass) -> f64 {
    match class {
        RoadClass::Motorway | RoadClass::MotorwayLink | RoadClass::Trunk | RoadClass::TrunkLink => 8.0,
        _ => 7.5,
    }
}

pub fn jam_density_per_lane(class: RoadClass) -> f64 {
    1.0 / jam_spacing_m(class)
}

pub fn wave_speed_mps(class: RoadClass) -> f64 {
    match class {
        RoadClass::Motorway | RoadClass::MotorwayLink | RoadClass::Trunk | RoadClass::TrunkLink => 5.56,
        _ => 5.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_fallback_is_fifty_kilometres_per_hour() {
        let speed = default_speed_mps(RoadClass::Primary);
        assert!((speed * 3.6 - 50.0).abs() < 1e-9);
    }

    #[test]
    fn jam_density_is_the_reciprocal_of_spacing() {
        let density = jam_density_per_lane(RoadClass::Residential);
        assert!((density - 1.0 / 7.5).abs() < 1e-12);
    }
}
