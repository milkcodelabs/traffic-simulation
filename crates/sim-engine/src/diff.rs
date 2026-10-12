//! Link-by-link comparison of a baseline run and an intervention run.
//!
//! Improved means the intervention is faster or less delayed. Degraded means the opposite.
//! A link neither run used, or whose change is inside the band, stays neutral.

use crate::EdgeObservation;

pub const DIFF_NEUTRAL: u8 = 0;
pub const DIFF_IMPROVED: u8 = 1;
pub const DIFF_DEGRADED: u8 = 2;

/// Speed changes inside 5% of free-flow, and delay changes inside 1 second, are neutral.
/// When both move, the combined score decides the colour: positive speed and negative delay help.
pub fn compare_edge(base: EdgeObservation, test: EdgeObservation, free_flow_mps: f64) -> u8 {
    if !base.observed() && !test.observed() && base.delay_s <= 1e-9 && test.delay_s <= 1e-9 {
        return DIFF_NEUTRAL;
    }
    let free = free_flow_mps.max(1.0);
    let base_speed = if base.observed() {
        base.mean_speed_mps
    } else {
        free
    };
    let test_speed = if test.observed() {
        test.mean_speed_mps
    } else {
        free
    };
    let speed_delta = test_speed - base_speed;
    let delay_delta = test.delay_s - base.delay_s;
    let speed_hit = speed_delta.abs() > 0.05 * free;
    let delay_hit = delay_delta.abs() > 1.0;
    if !speed_hit && !delay_hit {
        return DIFF_NEUTRAL;
    }
    let score = speed_delta / free - delay_delta / 30.0;
    if score > 1e-9 {
        DIFF_IMPROVED
    } else if score < -1e-9 {
        DIFF_DEGRADED
    } else {
        DIFF_NEUTRAL
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Distribution {
    pub n: usize,
    pub mean: f64,
    pub variance: f64,
    pub ci95_low: f64,
    pub ci95_high: f64,
}

/// Sample mean, unbiased sample variance, and a 95% Student-t interval for the mean.
pub fn distribution(samples: &[f64]) -> Distribution {
    let n = samples.len();
    if n == 0 {
        return Distribution {
            n: 0,
            mean: 0.0,
            variance: 0.0,
            ci95_low: 0.0,
            ci95_high: 0.0,
        };
    }
    let mean = samples.iter().sum::<f64>() / n as f64;
    let variance = if n < 2 {
        0.0
    } else {
        samples
            .iter()
            .map(|value| {
                let delta = value - mean;
                delta * delta
            })
            .sum::<f64>()
            / (n - 1) as f64
    };
    let half = t_crit_975(n.saturating_sub(1)) * (variance / n as f64).sqrt();
    Distribution {
        n,
        mean,
        variance,
        ci95_low: mean - half,
        ci95_high: mean + half,
    }
}

fn t_crit_975(df: usize) -> f64 {
    const TABLE: [f64; 30] = [
        12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
        2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
        2.052, 2.048, 2.045, 2.042,
    ];
    if df == 0 {
        0.0
    } else if df <= TABLE.len() {
        TABLE[df - 1]
    } else {
        1.960
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(delay: f64, speed: f64) -> EdgeObservation {
        EdgeObservation {
            delay_s: delay,
            mean_speed_mps: speed,
        }
    }

    #[test]
    fn colours_follow_speed_and_delay() {
        let free = 10.0;
        assert_eq!(
            compare_edge(obs(0.0, f64::NAN), obs(0.0, f64::NAN), free),
            DIFF_NEUTRAL
        );
        assert_eq!(
            compare_edge(obs(0.0, 10.0), obs(0.0, 10.1), free),
            DIFF_NEUTRAL
        );
        assert_eq!(
            compare_edge(obs(0.0, 8.0), obs(0.0, 10.0), free),
            DIFF_IMPROVED
        );
        assert_eq!(
            compare_edge(obs(0.0, 10.0), obs(40.0, 10.0), free),
            DIFF_DEGRADED
        );
        assert_eq!(
            compare_edge(obs(20.0, 8.0), obs(0.0, 10.0), free),
            DIFF_IMPROVED
        );
    }

    #[test]
    fn ten_samples_use_the_student_interval() {
        let samples = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let summary = distribution(&samples);
        assert_eq!(summary.n, 10);
        assert!((summary.mean - 5.5).abs() < 1e-12);
        assert!(summary.variance > 0.0);
        assert!(summary.ci95_low < summary.mean && summary.mean < summary.ci95_high);
        assert!((t_crit_975(9) - 2.262).abs() < 1e-9);
    }
}
