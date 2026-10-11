use serde::{Deserialize, Serialize};

/// Default urban heavy-vehicle share used by the uphill saturation penalty.
pub const DEFAULT_HEAVY_VEHICLE_SHARE: f64 = 0.08;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeSample {
    /// Length-weighted mean grade in radians. Positive is uphill in the direction of travel.
    pub mean_grade_rad: f64,
    /// Length-weighted mean of the positive part of segment grade.
    pub uphill_grade_rad: f64,
}

impl GradeSample {
    pub const FLAT: Self = Self {
        mean_grade_rad: 0.0,
        uphill_grade_rad: 0.0,
    };
}

/// Ground elevation source. Bridge and tunnel decks are not sampled from this trait;
/// the compiler passes `ignore_grade` for those edges because a ground raster is not the deck.
pub trait ElevationSampler: Send + Sync {
    fn sample(&self, lon_deg: f64, lat_deg: f64) -> Option<f64>;
}

/// Constant height. Grade on a connected polyline is zero, which is flat terrain.
#[derive(Clone, Copy, Debug)]
pub struct FlatElevation {
    pub elevation_m: f64,
}

impl ElevationSampler for FlatElevation {
    fn sample(&self, _lon_deg: f64, _lat_deg: f64) -> Option<f64> {
        Some(self.elevation_m)
    }
}

/// Row-major raster in geographic degrees. `NaN` cells are missing and return `None`.
#[derive(Clone, Debug)]
pub struct GridElevation {
    pub origin_lon: f64,
    pub origin_lat: f64,
    pub cell_lon_deg: f64,
    pub cell_lat_deg: f64,
    pub cols: usize,
    pub rows: usize,
    pub values: Vec<f32>,
}

impl GridElevation {
    pub fn sample_bilinear(&self, lon_deg: f64, lat_deg: f64) -> Option<f64> {
        if self.cols < 2 || self.rows < 2 || self.cell_lon_deg == 0.0 || self.cell_lat_deg == 0.0 {
            return None;
        }
        let col = (lon_deg - self.origin_lon) / self.cell_lon_deg;
        let row = (lat_deg - self.origin_lat) / self.cell_lat_deg;
        if col < 0.0 || row < 0.0 || col > (self.cols - 1) as f64 || row > (self.rows - 1) as f64 {
            return None;
        }
        let c0 = col.floor() as usize;
        let r0 = row.floor() as usize;
        let c1 = (c0 + 1).min(self.cols - 1);
        let r1 = (r0 + 1).min(self.rows - 1);
        let tx = if c0 == c1 { 0.0 } else { col - c0 as f64 };
        let ty = if r0 == r1 { 0.0 } else { row - r0 as f64 };
        let v00 = self.cell(c0, r0)?;
        let v10 = self.cell(c1, r0)?;
        let v01 = self.cell(c0, r1)?;
        let v11 = self.cell(c1, r1)?;
        let top = v00 * (1.0 - tx) + v10 * tx;
        let bottom = v01 * (1.0 - tx) + v11 * tx;
        Some(top * (1.0 - ty) + bottom * ty)
    }

    fn cell(&self, col: usize, row: usize) -> Option<f64> {
        let value = *self.values.get(row * self.cols + col)?;
        if value.is_finite() {
            Some(f64::from(value))
        } else {
            None
        }
    }
}

impl ElevationSampler for GridElevation {
    fn sample(&self, lon_deg: f64, lat_deg: f64) -> Option<f64> {
        self.sample_bilinear(lon_deg, lat_deg)
    }
}

/// Length-weighted grades. A missing sample on either end of a segment contributes a flat segment
/// of that length, so unknown elevation does not invent a slope.
pub fn grades_from_profile(horizontal_m: &[f64], elevation_m: &[Option<f64>]) -> GradeSample {
    if horizontal_m.len() + 1 != elevation_m.len() {
        return GradeSample::FLAT;
    }
    let mut length = 0.0;
    let mut mean_acc = 0.0;
    let mut uphill_acc = 0.0;
    for (index, segment) in horizontal_m.iter().copied().enumerate() {
        if segment <= 0.0 {
            continue;
        }
        let grade = match (elevation_m[index], elevation_m[index + 1]) {
            (Some(start), Some(end)) => (end - start).atan2(segment),
            _ => 0.0,
        };
        length += segment;
        mean_acc += grade * segment;
        uphill_acc += grade.max(0.0) * segment;
    }
    if length <= 0.0 {
        GradeSample::FLAT
    } else {
        GradeSample {
            mean_grade_rad: mean_acc / length,
            uphill_grade_rad: uphill_acc / length,
        }
    }
}

pub fn speed_grade_factor(uphill_grade_rad: f64) -> f64 {
    let rise_over_run = uphill_grade_rad.tan().max(0.0);
    1.0 / (1.0 + 0.5 * rise_over_run)
}

pub fn saturation_capacity_factor(uphill_grade_rad: f64, heavy_share: f64) -> f64 {
    let heavy = heavy_share.clamp(0.0, 1.0);
    let rise_over_run = uphill_grade_rad.tan().max(0.0);
    let alpha = 1.5 * (1.0 - heavy) + 6.0 * heavy;
    1.0 / (1.0 + alpha * rise_over_run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_percent_grade_matches_the_mixed_fleet_factors() {
        let grade = (50.0_f64).atan2(1_000.0);
        let sample = grades_from_profile(&[1_000.0], &[Some(0.0), Some(50.0)]);
        assert!((sample.mean_grade_rad - grade).abs() < 1e-12);
        assert!((sample.uphill_grade_rad - grade).abs() < 1e-12);
        let speed = speed_grade_factor(grade);
        let capacity = saturation_capacity_factor(grade, DEFAULT_HEAVY_VEHICLE_SHARE);
        assert!((speed - 1.0 / 1.025).abs() < 1e-12);
        let alpha = 1.5 * 0.92 + 6.0 * 0.08;
        assert!((capacity - 1.0 / (1.0 + alpha * 0.05)).abs() < 1e-12);
    }

    #[test]
    fn downhill_does_not_raise_capacity() {
        let downhill = (-40.0_f64).atan2(1_000.0);
        assert!((saturation_capacity_factor(downhill, 0.08) - 1.0).abs() < 1e-12);
        assert!((speed_grade_factor(downhill) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn bilinear_grid_reads_the_cell_centre() {
        let grid = GridElevation {
            origin_lon: 22.0,
            origin_lat: 40.0,
            cell_lon_deg: 0.1,
            cell_lat_deg: 0.1,
            cols: 2,
            rows: 2,
            values: vec![0.0, 10.0, 20.0, 30.0],
        };
        let mid = grid.sample(22.05, 40.05).unwrap();
        assert!((mid - 15.0).abs() < 1e-9);
    }
}
