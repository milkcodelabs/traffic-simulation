use std::path::Path;

use osmpbf::{Element, ElementReader, RelMemberType};
use rustc_hash::FxHashMap;

use crate::error::IngestError;
use crate::model::{OsmDocument, OsmNode, OsmRestriction, OsmWay};

pub fn parse_pbf_file(path: &Path) -> Result<OsmDocument, IngestError> {
    let reader = ElementReader::from_path(path).map_err(|error| IngestError::Pbf(error.to_string()))?;
    let mut document = OsmDocument::default();
    reader
        .for_each(|element| {
            match element {
                Element::Node(node) => store_node(&mut document, node.id(), node.lat(), node.lon(), node.tags()),
                Element::DenseNode(node) => store_node(&mut document, node.id(), node.lat(), node.lon(), node.tags()),
                Element::Way(way) => {
                    let mut tags = FxHashMap::default();
                    for (key, value) in way.tags() {
                        tags.insert(key.to_string(), value.to_string());
                    }
                    if let Some(highway) = tags.get("highway").cloned() {
                        let nodes = way.refs().filter_map(|id| u64::try_from(id).ok()).collect::<Vec<_>>();
                        if way.id() > 0 && nodes.len() >= 2 {
                            document.ways.push(way_from_tags(way.id() as u64, nodes, highway, &tags));
                        }
                    }
                }
                Element::Relation(relation) => {
                    let mut tags = FxHashMap::default();
                    for (key, value) in relation.tags() {
                        tags.insert(key.to_string(), value.to_string());
                    }
                    if tags.get("type").map(String::as_str) != Some("restriction") {
                        return;
                    }
                    let Some(restriction) = tags.get("restriction").cloned() else {
                        return;
                    };
                    let mut from_way = None;
                    let mut to_way = None;
                    let mut via_node = None;
                    let mut via_way = None;
                    for member in relation.members() {
                        let role = member.role().unwrap_or("");
                        let id = u64::try_from(member.member_id).unwrap_or(0);
                        match (role, member.member_type) {
                            ("from", RelMemberType::Way) => from_way = Some(id),
                            ("to", RelMemberType::Way) => to_way = Some(id),
                            ("via", RelMemberType::Node) => via_node = Some(id),
                            ("via", RelMemberType::Way) => via_way = Some(id),
                            _ => {}
                        }
                    }
                    if let (Some(from_way), Some(to_way)) = (from_way, to_way) {
                        if via_node.is_some() || via_way.is_some() {
                            document.restrictions.push(OsmRestriction {
                                from_way,
                                to_way,
                                via_node,
                                via_way,
                                restriction,
                            });
                        }
                    }
                }
            }
        })
        .map_err(|error| IngestError::Pbf(error.to_string()))?;
    Ok(document)
}

fn store_node<'a>(
    document: &mut OsmDocument,
    id: i64,
    lat: f64,
    lon: f64,
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) {
    let Ok(id) = u64::try_from(id) else {
        return;
    };
    let mut signal = false;
    let mut mini_roundabout = false;
    let mut motorway_junction = false;
    for (key, value) in tags {
        match (key, value) {
            ("highway", "traffic_signals") | ("crossing", "traffic_signals") => signal = true,
            ("highway", "mini_roundabout") => mini_roundabout = true,
            ("highway", "motorway_junction") => motorway_junction = true,
            _ => {}
        }
    }
    document.nodes.insert(
        id,
        OsmNode {
            lon,
            lat,
            signal,
            mini_roundabout,
            motorway_junction,
        },
    );
}

fn way_from_tags(id: u64, nodes: Vec<u64>, highway: String, tags: &FxHashMap<String, String>) -> OsmWay {
    let tag = |key: &str| tags.get(key).cloned();
    let flag = |key: &str| tags.get(key).is_some_and(|value| !matches!(value.as_str(), "no" | "false" | "0"));
    OsmWay {
        id,
        nodes,
        highway,
        oneway: tag("oneway"),
        lanes: tag("lanes"),
        lanes_forward: tag("lanes:forward"),
        lanes_backward: tag("lanes:backward"),
        maxspeed: tag("maxspeed"),
        name: tag("name"),
        road_ref: tag("ref"),
        junction: tag("junction"),
        bridge: flag("bridge"),
        tunnel: flag("tunnel"),
        access: tag("access"),
        motor_vehicle: tag("motor_vehicle"),
        motorcar: tag("motorcar"),
        service: tag("service"),
        area: tags.get("area").is_some_and(|value| matches!(value.as_str(), "yes" | "true" | "1")),
    }
}
