use sim_core::Network;

/// Little-endian viewport blob. `f32` degrees are for drawing only.
pub const VIEWPORT_MAGIC: u32 = 0x5453_4E31;

pub fn encode_viewport(network: &Network) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&VIEWPORT_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&(network.edges.len() as u32).to_le_bytes());
    for (index, edge) in network.edges.iter().enumerate() {
        bytes.extend_from_slice(&(index as u32).to_le_bytes());
        bytes.push(edge.class.as_u8());
        bytes.push(edge.lanes);
        let speed_kph = (edge.free_flow_speed_mps * 3.6).round().clamp(0.0, 65535.0) as u16;
        bytes.extend_from_slice(&speed_kph.to_le_bytes());
        let count = u16::try_from(edge.geometry_lonlat.len()).unwrap_or(u16::MAX);
        bytes.extend_from_slice(&count.to_le_bytes());
        for point in edge.geometry_lonlat.iter().take(count as usize) {
            bytes.extend_from_slice(&(point.lon_deg as f32).to_le_bytes());
            bytes.extend_from_slice(&(point.lat_deg as f32).to_le_bytes());
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{
        AssembleInput, BBox, DiagramFamily, EdgeDraft, EdgeFlags, Node, NodeKind, RoadClass, UtmProjection,
        DEFAULT_HEAVY_VEHICLE_SHARE,
    };

    #[test]
    fn viewport_blob_round_trips_the_edge_header() {
        let bbox = BBox {
            min_lat: 40.5,
            min_lon: 22.8,
            max_lat: 40.7,
            max_lon: 23.1,
        };
        let projection = UtmProjection::for_bbox(bbox);
        let start = sim_core::LonLat {
            lon_deg: 22.94,
            lat_deg: 40.63,
        };
        let end = sim_core::LonLat {
            lon_deg: 22.95,
            lat_deg: 40.63,
        };
        let edge = sim_core::realize_edge(EdgeDraft {
            from: sim_core::NodeId(0),
            to: sim_core::NodeId(1),
            geometry_xy: vec![projection.forward(start), projection.forward(end)],
            geometry_lonlat: vec![start, end],
            elevations_m: vec![Some(0.0), Some(0.0)],
            ignore_grade: true,
            class: RoadClass::Primary,
            lanes: 2,
            posted_speed_mps: 50.0 / 3.6,
            osm_way_ids: vec![1],
            name: None,
            road_ref: None,
            flags: EdgeFlags::default(),
            diagram: DiagramFamily::Triangular,
            heavy_vehicle_share: DEFAULT_HEAVY_VEHICLE_SHARE,
        })
        .unwrap();
        let node = |lonlat| Node {
            kind: NodeKind::Intersection,
            lonlat,
            xy: projection.forward(lonlat),
            elevation_m: Some(0.0),
            osm_id: 1,
            synthetic: false,
            signal: None,
        };
        let network = sim_core::Network::assemble(AssembleInput {
            preset_id: None,
            bbox,
            projection,
            diagram: DiagramFamily::Triangular,
            nodes: vec![node(start), node(end)],
            edges: vec![edge],
            turns: Vec::new(),
            signals: Vec::new(),
            zones: Vec::new(),
            contractions: 0,
            compiled_unix_s: 0,
        })
        .unwrap();
        let bytes = encode_viewport(&network);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), VIEWPORT_MAGIC);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1);
        assert_eq!(bytes[12], RoadClass::Primary.as_u8());
        assert_eq!(bytes[13], 2);
        assert_eq!(u16::from_le_bytes(bytes[16..18].try_into().unwrap()), 2);
    }
}
