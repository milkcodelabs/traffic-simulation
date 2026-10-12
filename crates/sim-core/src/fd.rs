use serde::{Deserialize, Serialize};

/// Diagram family stored on the compiled network.
///
/// `critical_speed_ratio` is `u_c / u_f`. A ratio of 1 reproduces the triangular diagram.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagramFamily {
    Triangular,
    Greenshields,
    Smulders { critical_speed_ratio: f64 },
}

impl DiagramFamily {
    pub fn name(self) -> &'static str {
        match self {
            Self::Triangular => "triangular",
            Self::Greenshields => "greenshields",
            Self::Smulders { .. } => "smulders",
        }
    }
}

/// Textbook triangular capacity, vehicles per second on the link.
pub fn triangular_capacity(free_flow_mps: f64, wave_mps: f64, jam_veh_per_m: f64) -> f64 {
    if free_flow_mps <= 0.0 || wave_mps <= 0.0 || jam_veh_per_m <= 0.0 {
        return 0.0;
    }
    free_flow_mps * jam_veh_per_m * wave_mps / (free_flow_mps + wave_mps)
}

/// Newell / Daganzo triangle. `jam_veh_per_m` is the link jam density.
pub fn triangular_flow(free_flow_mps: f64, wave_mps: f64, jam_veh_per_m: f64, density: f64) -> f64 {
    if density <= 0.0 || density >= jam_veh_per_m || free_flow_mps <= 0.0 || wave_mps <= 0.0 {
        return 0.0;
    }
    let k_c = jam_veh_per_m * wave_mps / (free_flow_mps + wave_mps);
    if density <= k_c {
        free_flow_mps * density
    } else {
        wave_mps * (jam_veh_per_m - density)
    }
}

/// Greenshields (1935): linear speed-density, parabolic flow-density.
pub fn greenshields_flow(free_flow_mps: f64, jam_veh_per_m: f64, density: f64) -> f64 {
    if density <= 0.0 || density >= jam_veh_per_m || free_flow_mps <= 0.0 || jam_veh_per_m <= 0.0 {
        return 0.0;
    }
    free_flow_mps * density * (1.0 - density / jam_veh_per_m)
}

/// Smulders (1990) in the link-transmission form: linear free-flow speed-density,
/// linear congested flow-density. `capacity` is `q_c` and `critical_speed` is `u_c`.
pub fn smulders_flow(
    free_flow_mps: f64,
    critical_speed_mps: f64,
    capacity_veh_per_s: f64,
    jam_veh_per_m: f64,
    density: f64,
) -> f64 {
    if density <= 0.0
        || density >= jam_veh_per_m
        || free_flow_mps <= 0.0
        || critical_speed_mps <= 0.0
        || capacity_veh_per_s <= 0.0
    {
        return 0.0;
    }
    let k_c = capacity_veh_per_s / critical_speed_mps;
    if k_c <= 0.0 || k_c >= jam_veh_per_m {
        return 0.0;
    }
    if density <= k_c {
        let speed = free_flow_mps - (free_flow_mps - critical_speed_mps) * density / k_c;
        density * speed
    } else {
        capacity_veh_per_s * (jam_veh_per_m - density) / (jam_veh_per_m - k_c)
    }
}

/// Flow on a compiled edge. Jam density is not reduced by grade.
///
/// The triangular branch is rebuilt from the edge's adjusted free-flow speed and
/// penalized capacity. Greenshields is scaled so its peak matches that capacity.
/// Smulders uses the penalized capacity as `q_c`.
pub fn equilibrium_flow(
    family: DiagramFamily,
    free_flow_mps: f64,
    capacity_veh_per_s: f64,
    jam_veh_per_m: f64,
    density: f64,
) -> f64 {
    if density <= 0.0 || density >= jam_veh_per_m || free_flow_mps <= 0.0 || jam_veh_per_m <= 0.0 {
        return 0.0;
    }
    match family {
        DiagramFamily::Triangular => {
            let k_c = capacity_veh_per_s / free_flow_mps;
            if k_c <= 0.0 || k_c >= jam_veh_per_m || capacity_veh_per_s <= 0.0 {
                return 0.0;
            }
            if density <= k_c {
                free_flow_mps * density
            } else {
                capacity_veh_per_s * (jam_veh_per_m - density) / (jam_veh_per_m - k_c)
            }
        }
        DiagramFamily::Greenshields => {
            let classical_peak = free_flow_mps * jam_veh_per_m / 4.0;
            if classical_peak <= 0.0 {
                return 0.0;
            }
            let scale = capacity_veh_per_s / classical_peak;
            scale * greenshields_flow(free_flow_mps, jam_veh_per_m, density)
        }
        DiagramFamily::Smulders {
            critical_speed_ratio,
        } => {
            let critical_speed = free_flow_mps * critical_speed_ratio;
            smulders_flow(
                free_flow_mps,
                critical_speed,
                capacity_veh_per_s,
                jam_veh_per_m,
                density,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) {
        let scale = expected.abs().max(1.0);
        assert!(
            (actual - expected).abs() <= 1e-9 * scale,
            "actual {actual} expected {expected}"
        );
    }

    #[test]
    fn triangular_endpoints_and_capacity() {
        let v = 15.0;
        let w = 5.0;
        let jam = 0.4;
        let q_max = triangular_capacity(v, w, jam);
        let k_c = jam * w / (v + w);
        close(q_max, v * k_c);
        close(triangular_flow(v, w, jam, 0.0), 0.0);
        close(triangular_flow(v, w, jam, jam), 0.0);
        close(triangular_flow(v, w, jam, k_c), q_max);
        close(triangular_flow(v, w, jam, k_c * 0.5), v * k_c * 0.5);
        close(triangular_flow(v, w, jam, -1.0), 0.0);
    }

    #[test]
    fn greenshields_peaks_at_half_jam() {
        let v = 20.0;
        let jam = 0.15;
        let k_c = jam / 2.0;
        let q_max = v * jam / 4.0;
        close(greenshields_flow(v, jam, 0.0), 0.0);
        close(greenshields_flow(v, jam, jam), 0.0);
        close(greenshields_flow(v, jam, k_c), q_max);
    }

    #[test]
    fn smulders_is_continuous_and_triangular_when_speeds_match() {
        let v = 25.0;
        let w = 5.56;
        let jam = 0.25;
        let q = triangular_capacity(v, w, jam);
        let k_c = q / v;
        close(smulders_flow(v, v, q, jam, 0.0), 0.0);
        close(smulders_flow(v, v, q, jam, jam), 0.0);
        close(smulders_flow(v, v, q, jam, k_c), q);
        close(
            smulders_flow(v, v, q, jam, k_c * 0.4),
            triangular_flow(v, w, jam, k_c * 0.4),
        );

        let u_c = 0.85 * v;
        let k_sm = q / u_c;
        close(smulders_flow(v, u_c, q, jam, k_sm), q);
        let below = smulders_flow(v, u_c, q, jam, k_sm - 1e-6);
        let above = smulders_flow(v, u_c, q, jam, k_sm + 1e-6);
        assert!((below - q).abs() < 1e-3);
        assert!((above - q).abs() < 1e-3);
    }
}
