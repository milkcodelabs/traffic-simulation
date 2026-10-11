use serde::{Deserialize, Serialize};

/// Drivers replace a path only when experienced travel time exceeds this times the baseline.
/// The assignment step reads the constant; no paths are stored yet.
pub const DEFAULT_ROUTE_INERTIA: f64 = 1.4;

/// Twenty-four hourly fractions of daily demand. The fractions sum to 1.
/// Trip productions are not part of this profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiurnalProfile {
    pub hour_fraction: [f64; 24],
}

impl DiurnalProfile {
    /// Normalized mixture of a night floor and three Gaussians.
    ///
    /// Weights: night 0.08, morning 0.28 at 08:00 (σ = 1.2 h),
    /// midday 0.34 at 13:00 (σ = 2.5 h), evening 0.30 at 17:30 (σ = 1.4 h).
    pub fn urban_default() -> Self {
        let mut raw = [0.0; 24];
        for hour in 0..24 {
            let t = hour as f64 + 0.5;
            let night = 0.08 / 24.0;
            let morning = 0.28 * gaussian(t, 8.0, 1.2);
            let midday = 0.34 * gaussian(t, 13.0, 2.5);
            let evening = 0.30 * gaussian(t, 17.5, 1.4);
            raw[hour] = night + morning + midday + evening;
        }
        let sum: f64 = raw.iter().sum();
        let hour_fraction = raw.map(|value| value / sum);
        Self { hour_fraction }
    }

    pub fn sum(&self) -> f64 {
        self.hour_fraction.iter().sum()
    }
}

fn gaussian(t: f64, mean: f64, sigma: f64) -> f64 {
    let z = (t - mean) / sigma;
    (-0.5 * z * z).exp() / (sigma * (std::f64::consts::TAU).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urban_profile_sums_to_one_and_peaks_above_night() {
        let profile = DiurnalProfile::urban_default();
        assert!((profile.sum() - 1.0).abs() < 1e-12);
        let night = profile.hour_fraction[2];
        let morning = profile.hour_fraction[8];
        let evening = profile.hour_fraction[17];
        assert!(morning > night * 2.0);
        assert!(evening > night * 2.0);
        assert!((DEFAULT_ROUTE_INERTIA - 1.4).abs() < 1e-12);
        assert!((1.3..=1.5).contains(&DEFAULT_ROUTE_INERTIA));
    }
}
