use serde::{Deserialize, Serialize};

use crate::geo::{LonLat, Xy};
use crate::BBox;

/// WGS84 ellipsoid constants used by the Redfearn / Snyder UTM series.
const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_223_563;
const K0: f64 = 0.9996;
const E2: f64 = F * (2.0 - F);
const EP2: f64 = E2 / (1.0 - E2);

/// UTM zone of the network centroid. Norway and Svalbard zone exceptions are not applied;
/// the calibration city and ordinary mid-latitude boxes do not use them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UtmProjection {
    pub zone: u8,
    pub northern: bool,
}

impl UtmProjection {
    pub fn for_bbox(bbox: BBox) -> Self {
        let centre = bbox.centroid();
        Self::for_lonlat(centre)
    }

    pub fn for_lonlat(point: LonLat) -> Self {
        Self {
            zone: zone_for_longitude(point.lon_deg),
            northern: point.lat_deg >= 0.0,
        }
    }

    pub fn central_meridian_rad(self) -> f64 {
        let degrees = f64::from(self.zone) * 6.0 - 183.0;
        degrees.to_radians()
    }

    pub fn forward(self, point: LonLat) -> Xy {
        let lat = point.lat_deg.to_radians();
        let lon = point.lon_deg.to_radians();
        let sin_lat = lat.sin();
        let cos_lat = lat.cos();
        let tan_lat = lat.tan();
        let n = A / (1.0 - E2 * sin_lat * sin_lat).sqrt();
        let t = tan_lat * tan_lat;
        let c = EP2 * cos_lat * cos_lat;
        let a = (lon - self.central_meridian_rad()) * cos_lat;
        let m = meridional_arc(lat);

        let easting = K0
            * n
            * (a + (1.0 - t + c) * a.powi(3) / 6.0
                + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * EP2) * a.powi(5) / 120.0)
            + 500_000.0;
        let northing = K0
            * (m
                + n * tan_lat
                    * (a * a / 2.0
                        + (5.0 - t + 9.0 * c + 4.0 * c * c) * a.powi(4) / 24.0
                        + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * EP2) * a.powi(6) / 720.0));
        let northing = if self.northern {
            northing
        } else {
            northing + 10_000_000.0
        };
        Xy {
            x_m: easting,
            y_m: northing,
        }
    }

    pub fn inverse(self, point: Xy) -> LonLat {
        let e4 = E2 * E2;
        let e6 = e4 * E2;
        let sqrt_one_minus_e2 = (1.0 - E2).sqrt();
        let e1 = (1.0 - sqrt_one_minus_e2) / (1.0 + sqrt_one_minus_e2);

        let x = point.x_m - 500_000.0;
        let y = if self.northern {
            point.y_m
        } else {
            point.y_m - 10_000_000.0
        };
        let m = y / K0;
        let mu = m / (A * (1.0 - E2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0));
        let lat1 = mu
            + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
            + (21.0 * e1.powi(2) / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
            + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin()
            + (1097.0 * e1.powi(4) / 512.0) * (8.0 * mu).sin();

        let sin1 = lat1.sin();
        let cos1 = lat1.cos();
        let tan1 = lat1.tan();
        let n1 = A / (1.0 - E2 * sin1 * sin1).sqrt();
        let t1 = tan1 * tan1;
        let c1 = EP2 * cos1 * cos1;
        let r1 = A * (1.0 - E2) / (1.0 - E2 * sin1 * sin1).powf(1.5);
        let d = x / (n1 * K0);

        let lat = lat1
            - (n1 * tan1 / r1)
                * (d * d / 2.0
                    - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * EP2) * d.powi(4) / 24.0
                    + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 252.0 * EP2 - 3.0 * c1 * c1)
                        * d.powi(6)
                        / 720.0);
        let lon = self.central_meridian_rad()
            + (d - (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0
                + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * EP2 + 24.0 * t1 * t1)
                    * d.powi(5)
                    / 120.0)
                / cos1;

        LonLat {
            lon_deg: lon.to_degrees(),
            lat_deg: lat.to_degrees(),
        }
    }
}

pub fn zone_for_longitude(lon_deg: f64) -> u8 {
    let mut zone = ((lon_deg + 180.0) / 6.0).floor() as i32 + 1;
    zone = zone.clamp(1, 60);
    zone as u8
}

fn meridional_arc(lat_rad: f64) -> f64 {
    let e4 = E2 * E2;
    let e6 = e4 * E2;
    A * ((1.0 - E2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0) * lat_rad
        - (3.0 * E2 / 8.0 + 3.0 * e4 / 32.0 + 45.0 * e6 / 1024.0) * (2.0 * lat_rad).sin()
        + (15.0 * e4 / 256.0 + 45.0 * e6 / 1024.0) * (4.0 * lat_rad).sin()
        - (35.0 * e6 / 3072.0) * (6.0 * lat_rad).sin())
}

/// Metres from an interior point to the nearest edge of `bbox`, measured in the UTM plane
/// along a parallel or a meridian. Points outside the box report 0.
pub fn boundary_distance_m(bbox: BBox, point: LonLat, projection: UtmProjection) -> f64 {
    if !bbox.contains(point) {
        return 0.0;
    }
    let here = projection.forward(point);
    let samples = [
        LonLat {
            lon_deg: bbox.min_lon,
            lat_deg: point.lat_deg,
        },
        LonLat {
            lon_deg: bbox.max_lon,
            lat_deg: point.lat_deg,
        },
        LonLat {
            lon_deg: point.lon_deg,
            lat_deg: bbox.min_lat,
        },
        LonLat {
            lon_deg: point.lon_deg,
            lat_deg: bbox.max_lat,
        },
    ];
    samples
        .into_iter()
        .map(|sample| here.distance_to(projection.forward(sample)))
        .fold(f64::INFINITY, f64::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thessaloniki_is_zone_34_north() {
        let projection = UtmProjection::for_lonlat(LonLat {
            lon_deg: 22.95,
            lat_deg: 40.63,
        });
        assert_eq!(projection.zone, 34);
        assert!(projection.northern);
    }

    #[test]
    fn round_trip_stays_within_a_metre() {
        let original = LonLat {
            lon_deg: 22.9484,
            lat_deg: 40.6264,
        };
        let projection = UtmProjection::for_lonlat(original);
        let back = projection.inverse(projection.forward(original));
        let error = crate::geo::haversine_m(original, back);
        assert!(error < 1.0, "round trip error {error} m");
    }

    #[test]
    fn northbound_segment_matches_the_meridional_arc() {
        let start = LonLat {
            lon_deg: 22.95,
            lat_deg: 40.62,
        };
        let end = LonLat {
            lon_deg: 22.95,
            lat_deg: 40.63,
        };
        let projection = UtmProjection::for_lonlat(start);
        let projected = projection.forward(start).distance_to(projection.forward(end));
        let arc = (meridional_arc(end.lat_deg.to_radians()) - meridional_arc(start.lat_deg.to_radians())).abs();
        let relative = (projected - arc).abs() / arc;
        assert!(relative < 0.001, "relative length error {relative}");
    }
}
