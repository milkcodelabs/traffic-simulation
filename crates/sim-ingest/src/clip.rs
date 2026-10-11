use sim_core::{BBox, LonLat};

use crate::model::{is_synthetic, OsmNode, SYNTHETIC_NODE_BASE};

#[derive(Clone, Debug)]
pub struct ShapePoint {
    pub osm_id: u64,
    pub lonlat: LonLat,
}

pub fn clip_chain(
    points: &[ShapePoint],
    bbox: BBox,
    nodes: &mut rustc_hash::FxHashMap<u64, OsmNode>,
    next_synthetic: &mut u64,
) -> Vec<Vec<ShapePoint>> {
    if points.len() < 2 {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut current: Vec<ShapePoint> = Vec::new();
    for pair in points.windows(2) {
        match piece(&pair[0], &pair[1], bbox, nodes, next_synthetic) {
            Piece::Keep { end } => {
                if current.is_empty() {
                    current.push(pair[0].clone());
                }
                push_distinct(&mut current, end);
            }
            Piece::Span { start, end } => {
                flush(&mut parts, &mut current);
                current.push(start);
                push_distinct(&mut current, end);
            }
            Piece::Leave { end } => {
                if current.is_empty() {
                    current.push(pair[0].clone());
                }
                push_distinct(&mut current, end);
                flush(&mut parts, &mut current);
            }
            Piece::Drop => flush(&mut parts, &mut current),
        }
    }
    flush(&mut parts, &mut current);
    parts
}

enum Piece {
    Keep { end: ShapePoint },
    Span { start: ShapePoint, end: ShapePoint },
    Leave { end: ShapePoint },
    Drop,
}

fn piece(
    start: &ShapePoint,
    end: &ShapePoint,
    bbox: BBox,
    nodes: &mut rustc_hash::FxHashMap<u64, OsmNode>,
    next_synthetic: &mut u64,
) -> Piece {
    let start_in = bbox.contains(start.lonlat);
    let end_in = bbox.contains(end.lonlat);
    let hits = boundary_hits(start.lonlat, end.lonlat, bbox);
    match (start_in, end_in) {
        (true, true) => Piece::Keep { end: end.clone() },
        (true, false) => hits
            .first()
            .map(|(_, point)| Piece::Leave {
                end: synthetic(point, nodes, next_synthetic),
            })
            .unwrap_or(Piece::Drop),
        (false, true) => hits
            .last()
            .map(|(_, point)| Piece::Span {
                start: synthetic(point, nodes, next_synthetic),
                end: end.clone(),
            })
            .unwrap_or(Piece::Drop),
        (false, false) => {
            if hits.len() >= 2 {
                let mid = lerp(start.lonlat, end.lonlat, (hits[0].0 + hits[1].0) * 0.5);
                if bbox.contains(mid) {
                    return Piece::Span {
                        start: synthetic(&hits[0].1, nodes, next_synthetic),
                        end: synthetic(&hits[1].1, nodes, next_synthetic),
                    };
                }
            }
            Piece::Drop
        }
    }
}

fn synthetic(point: &LonLat, nodes: &mut rustc_hash::FxHashMap<u64, OsmNode>, next_synthetic: &mut u64) -> ShapePoint {
    if *next_synthetic < SYNTHETIC_NODE_BASE {
        *next_synthetic = SYNTHETIC_NODE_BASE;
    }
    let osm_id = *next_synthetic;
    *next_synthetic += 1;
    nodes.insert(
        osm_id,
        OsmNode {
            lon: point.lon_deg,
            lat: point.lat_deg,
            signal: false,
            mini_roundabout: false,
            motorway_junction: false,
        },
    );
    debug_assert!(is_synthetic(osm_id));
    ShapePoint {
        osm_id,
        lonlat: *point,
    }
}

fn boundary_hits(start: LonLat, end: LonLat, bbox: BBox) -> Vec<(f64, LonLat)> {
    let mut hits = Vec::new();
    let mut push = |t: f64, point: LonLat| {
        if t <= 1e-9 || t >= 1.0 - 1e-9 || !on_boundary(bbox, point) {
            return;
        }
        if hits.iter().any(|(existing, _): &(f64, LonLat)| (existing - t).abs() < 1e-9) {
            return;
        }
        hits.push((t, point));
    };
    let dlon = end.lon_deg - start.lon_deg;
    let dlat = end.lat_deg - start.lat_deg;
    if dlon.abs() > 1e-15 {
        for edge in [bbox.min_lon, bbox.max_lon] {
            let t = (edge - start.lon_deg) / dlon;
            push(
                t,
                LonLat {
                    lon_deg: edge,
                    lat_deg: start.lat_deg + t * dlat,
                },
            );
        }
    }
    if dlat.abs() > 1e-15 {
        for edge in [bbox.min_lat, bbox.max_lat] {
            let t = (edge - start.lat_deg) / dlat;
            push(
                t,
                LonLat {
                    lon_deg: start.lon_deg + t * dlon,
                    lat_deg: edge,
                },
            );
        }
    }
    hits.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    hits
}

fn on_boundary(bbox: BBox, point: LonLat) -> bool {
    let lon_span = point.lon_deg >= bbox.min_lon - 1e-9 && point.lon_deg <= bbox.max_lon + 1e-9;
    let lat_span = point.lat_deg >= bbox.min_lat - 1e-9 && point.lat_deg <= bbox.max_lat + 1e-9;
    let on_lon = (point.lon_deg - bbox.min_lon).abs() <= 1e-8 || (point.lon_deg - bbox.max_lon).abs() <= 1e-8;
    let on_lat = (point.lat_deg - bbox.min_lat).abs() <= 1e-8 || (point.lat_deg - bbox.max_lat).abs() <= 1e-8;
    (on_lon && lat_span) || (on_lat && lon_span)
}

fn lerp(start: LonLat, end: LonLat, t: f64) -> LonLat {
    LonLat {
        lon_deg: start.lon_deg + (end.lon_deg - start.lon_deg) * t,
        lat_deg: start.lat_deg + (end.lat_deg - start.lat_deg) * t,
    }
}

fn push_distinct(points: &mut Vec<ShapePoint>, point: ShapePoint) {
    if points.last().is_none_or(|last| last.osm_id != point.osm_id) {
        points.push(point);
    }
}

fn flush(parts: &mut Vec<Vec<ShapePoint>>, current: &mut Vec<ShapePoint>) {
    if current.len() >= 2 {
        parts.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashMap;

    #[test]
    fn a_segment_crossing_the_west_edge_gains_a_synthetic_gateway_vertex() {
        let bbox = BBox {
            min_lat: 40.60,
            min_lon: 22.93,
            max_lat: 40.70,
            max_lon: 23.00,
        };
        let points = vec![
            ShapePoint {
                osm_id: 1,
                lonlat: LonLat {
                    lon_deg: 22.90,
                    lat_deg: 40.63,
                },
            },
            ShapePoint {
                osm_id: 2,
                lonlat: LonLat {
                    lon_deg: 22.95,
                    lat_deg: 40.63,
                },
            },
        ];
        let mut nodes = FxHashMap::default();
        let mut next = SYNTHETIC_NODE_BASE;
        let parts = clip_chain(&points, bbox, &mut nodes, &mut next);
        assert_eq!(parts.len(), 1);
        assert!(is_synthetic(parts[0][0].osm_id));
        assert!((parts[0][0].lonlat.lon_deg - 22.93).abs() < 1e-9);
        assert_eq!(parts[0].last().unwrap().osm_id, 2);
    }
}
