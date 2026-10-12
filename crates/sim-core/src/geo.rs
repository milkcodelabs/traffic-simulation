use serde::{Deserialize, Serialize};

use crate::CoreError;

/// WGS84 geographic position in degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LonLat {
    pub lon_deg: f64,
    pub lat_deg: f64,
}

/// Projected position in metres. Axes are easting and northing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Xy {
    pub x_m: f64,
    pub y_m: f64,
}

impl Xy {
    pub fn distance_to(self, other: Xy) -> f64 {
        let dx = other.x_m - self.x_m;
        let dy = other.y_m - self.y_m;
        dx.hypot(dy)
    }
}

/// Inclusive geographic bounding box. Latitudes are degrees north, longitudes degrees east.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BBox {
    pub min_lat: f64,
    pub min_lon: f64,
    pub max_lat: f64,
    pub max_lon: f64,
}

impl BBox {
    pub fn validate(self) -> Result<Self, CoreError> {
        let lat_ok = (-90.0..=90.0).contains(&self.min_lat)
            && (-90.0..=90.0).contains(&self.max_lat)
            && self.min_lat < self.max_lat;
        let lon_ok = (-180.0..=180.0).contains(&self.min_lon)
            && (-180.0..=180.0).contains(&self.max_lon)
            && self.min_lon < self.max_lon;
        if lat_ok && lon_ok {
            Ok(self)
        } else {
            Err(CoreError::InvalidBBox)
        }
    }

    pub fn contains(self, point: LonLat) -> bool {
        point.lat_deg >= self.min_lat
            && point.lat_deg <= self.max_lat
            && point.lon_deg >= self.min_lon
            && point.lon_deg <= self.max_lon
    }

    pub fn centroid(self) -> LonLat {
        LonLat {
            lon_deg: (self.min_lon + self.max_lon) * 0.5,
            lat_deg: (self.min_lat + self.max_lat) * 0.5,
        }
    }
}

pub fn polyline_length_m(points: &[Xy]) -> f64 {
    points
        .windows(2)
        .map(|pair| pair[0].distance_to(pair[1]))
        .sum()
}

/// Great-circle distance on the WGS84 authalic sphere of radius 6_378_137 m.
pub fn haversine_m(a: LonLat, b: LonLat) -> f64 {
    const RADIUS_M: f64 = 6_378_137.0;
    let lat1 = a.lat_deg.to_radians();
    let lat2 = b.lat_deg.to_radians();
    let dlat = lat2 - lat1;
    let dlon = (b.lon_deg - a.lon_deg).to_radians();
    let h = (dlat * 0.5).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon * 0.5).sin().powi(2);
    2.0 * RADIUS_M * h.sqrt().asin()
}

/// Navigation bearing in radians: 0 is north, clockwise positive, range (−π, π].
pub fn bearing_rad(from: Xy, to: Xy) -> Option<f64> {
    let dx = to.x_m - from.x_m;
    let dy = to.y_m - from.y_m;
    if dx.hypot(dy) < 1e-6 {
        None
    } else {
        Some(dx.atan2(dy))
    }
}

pub fn wrap_pi(angle: f64) -> f64 {
    let full = std::f64::consts::TAU;
    let mut wrapped = (angle + std::f64::consts::PI) % full;
    if wrapped < 0.0 {
        wrapped += full;
    }
    wrapped - std::f64::consts::PI
}

/// First and last segments with a real length, for turn angles.
pub fn endpoint_bearings(polyline: &[Xy]) -> Option<(f64, f64)> {
    let mut departure = None;
    for pair in polyline.windows(2) {
        if let Some(bearing) = bearing_rad(pair[0], pair[1]) {
            departure = Some(bearing);
            break;
        }
    }
    let mut arrival = None;
    for pair in polyline.windows(2).rev() {
        if let Some(bearing) = bearing_rad(pair[0], pair[1]) {
            arrival = Some(bearing);
            break;
        }
    }
    match (departure, arrival) {
        (Some(departure), Some(arrival)) => Some((departure, arrival)),
        _ => None,
    }
}
