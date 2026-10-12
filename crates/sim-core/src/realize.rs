use thiserror::Error;

use crate::fd::{equilibrium_flow, triangular_capacity, DiagramFamily};
use crate::geo::{polyline_length_m, LonLat, Xy};
use crate::grade::{
    grades_from_profile, saturation_capacity_factor, speed_grade_factor, GradeSample,
};
use crate::graph::{Edge, EdgeFlags};
use crate::id::NodeId;
use crate::road::{jam_density_per_lane, wave_speed_mps, RoadClass};

#[derive(Debug, Error)]
pub enum RealizeError {
    #[error("edge is shorter than 1 m ({length_m:.3} m)")]
    Short { length_m: f64 },
    #[error("edge geometry does not match its elevation profile")]
    Geometry,
    #[error("{0}")]
    Diagram(String),
}

/// Inputs for one directed edge, before length, grade, and the fundamental diagram are applied.
pub struct EdgeDraft {
    pub from: NodeId,
    pub to: NodeId,
    pub geometry_xy: Vec<Xy>,
    pub geometry_lonlat: Vec<LonLat>,
    pub elevations_m: Vec<Option<f64>>,
    pub ignore_grade: bool,
    pub class: RoadClass,
    pub lanes: u8,
    pub posted_speed_mps: f64,
    pub osm_way_ids: Vec<u64>,
    pub name: Option<String>,
    pub road_ref: Option<String>,
    pub flags: EdgeFlags,
    pub diagram: DiagramFamily,
    pub heavy_vehicle_share: f64,
}

pub fn realize_edge(draft: EdgeDraft) -> Result<Edge, RealizeError> {
    if draft.geometry_xy.len() < 2
        || draft.geometry_xy.len() != draft.geometry_lonlat.len()
        || draft.elevations_m.len() != draft.geometry_xy.len()
        || draft.lanes == 0
        || draft.posted_speed_mps <= 0.0
        || draft.osm_way_ids.is_empty()
    {
        return Err(RealizeError::Geometry);
    }

    let length_m = polyline_length_m(&draft.geometry_xy);
    if length_m < 1.0 {
        return Err(RealizeError::Short { length_m });
    }

    let grades = if draft.ignore_grade {
        GradeSample::FLAT
    } else {
        let segments: Vec<f64> = draft
            .geometry_xy
            .windows(2)
            .map(|pair| pair[0].distance_to(pair[1]))
            .collect();
        grades_from_profile(&segments, &draft.elevations_m)
    };

    let jam_density_per_lane = jam_density_per_lane(draft.class);
    let jam_link = jam_density_per_lane * f64::from(draft.lanes);
    let wave = wave_speed_mps(draft.class);
    let base_capacity = textbook_capacity(draft.diagram, draft.posted_speed_mps, wave, jam_link)?;
    let speed_factor = speed_grade_factor(grades.uphill_grade_rad);
    let capacity_factor =
        saturation_capacity_factor(grades.uphill_grade_rad, draft.heavy_vehicle_share);
    let free_flow = draft.posted_speed_mps * speed_factor;
    let capacity = base_capacity * capacity_factor;
    if free_flow <= 0.0 || capacity < 0.0 {
        return Err(RealizeError::Diagram(
            "adjusted speed or capacity is not positive".into(),
        ));
    }

    let critical_speed = match draft.diagram {
        DiagramFamily::Triangular => free_flow,
        DiagramFamily::Greenshields => free_flow * 0.5,
        DiagramFamily::Smulders {
            critical_speed_ratio,
        } => free_flow * critical_speed_ratio,
    };
    if critical_speed <= 0.0 || capacity / critical_speed >= jam_link {
        return Err(RealizeError::Diagram(format!(
            "critical density {:.6} is not below jam density {:.6}",
            if critical_speed > 0.0 {
                capacity / critical_speed
            } else {
                f64::INFINITY
            },
            jam_link
        )));
    }

    let wave_speed = match draft.diagram {
        DiagramFamily::Triangular => {
            let k_c = capacity / free_flow;
            capacity / (jam_link - k_c)
        }
        _ => wave,
    };

    let storage_max_veh = f64::from(draft.lanes) * jam_density_per_lane * length_m;
    // Touch the operating evaluator once so a non-physical peak fails here rather than mid-run.
    let peak_density = match draft.diagram {
        DiagramFamily::Greenshields => jam_link * 0.5,
        _ => capacity / critical_speed,
    };
    let peak = equilibrium_flow(draft.diagram, free_flow, capacity, jam_link, peak_density);
    if (peak - capacity).abs() > 1e-6 * capacity.max(1.0) {
        return Err(RealizeError::Diagram(format!(
            "operating peak {peak} does not match penalized capacity {capacity}"
        )));
    }

    Ok(Edge {
        from: draft.from,
        to: draft.to,
        geometry_xy: draft.geometry_xy,
        geometry_lonlat: draft.geometry_lonlat,
        class: draft.class,
        lanes: draft.lanes,
        length_m,
        grade_rad: grades.mean_grade_rad,
        uphill_grade_rad: grades.uphill_grade_rad,
        free_flow_speed_mps: free_flow,
        base_free_flow_speed_mps: draft.posted_speed_mps,
        critical_speed_mps: critical_speed,
        capacity_veh_per_s: capacity,
        base_capacity_veh_per_s: base_capacity,
        jam_density_per_lane,
        wave_speed_mps: wave_speed,
        storage_max_veh,
        osm_way_ids: draft.osm_way_ids,
        name: draft.name,
        road_ref: draft.road_ref,
        flags: draft.flags,
    })
}

fn textbook_capacity(
    family: DiagramFamily,
    speed: f64,
    wave: f64,
    jam_link: f64,
) -> Result<f64, RealizeError> {
    let capacity = match family {
        DiagramFamily::Greenshields => speed * jam_link / 4.0,
        DiagramFamily::Triangular | DiagramFamily::Smulders { .. } => {
            triangular_capacity(speed, wave, jam_link)
        }
    };
    if capacity <= 0.0 {
        return Err(RealizeError::Diagram(
            "textbook capacity is not positive".into(),
        ));
    }
    Ok(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd::triangular_capacity;
    use crate::grade::DEFAULT_HEAVY_VEHICLE_SHARE;
    use crate::road::RoadClass;

    #[test]
    fn storage_and_uphill_penalty_follow_the_closed_forms() {
        let edge = realize_edge(EdgeDraft {
            from: NodeId(0),
            to: NodeId(1),
            geometry_xy: vec![
                Xy { x_m: 0.0, y_m: 0.0 },
                Xy {
                    x_m: 1000.0,
                    y_m: 0.0,
                },
            ],
            geometry_lonlat: vec![
                LonLat {
                    lon_deg: 22.9,
                    lat_deg: 40.6,
                },
                LonLat {
                    lon_deg: 22.91,
                    lat_deg: 40.6,
                },
            ],
            elevations_m: vec![Some(0.0), Some(50.0)],
            ignore_grade: false,
            class: RoadClass::Primary,
            lanes: 2,
            posted_speed_mps: 50.0 / 3.6,
            osm_way_ids: vec![7],
            name: None,
            road_ref: None,
            flags: EdgeFlags::default(),
            diagram: DiagramFamily::Triangular,
            heavy_vehicle_share: DEFAULT_HEAVY_VEHICLE_SHARE,
        })
        .unwrap();

        let jam = (1.0 / 7.5) * 2.0;
        let base = triangular_capacity(50.0 / 3.6, 5.0, jam);
        let alpha = 1.5 * 0.92 + 6.0 * 0.08;
        assert!((edge.length_m - 1000.0).abs() < 1e-9);
        assert!((edge.storage_max_veh - 2.0 * (1.0 / 7.5) * 1000.0).abs() < 1e-9);
        assert!((edge.base_capacity_veh_per_s - base).abs() < 1e-9);
        assert!((edge.capacity_veh_per_s - base / (1.0 + alpha * 0.05)).abs() < 1e-9);
        assert!((edge.free_flow_speed_mps - (50.0 / 3.6) / 1.025).abs() < 1e-9);
    }
}
