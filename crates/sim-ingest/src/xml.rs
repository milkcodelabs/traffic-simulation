use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;
use rustc_hash::FxHashMap;

use crate::error::IngestError;
use crate::model::{OsmDocument, OsmNode, OsmRestriction, OsmWay};

pub fn parse_xml_file(path: &Path) -> Result<OsmDocument, IngestError> {
    let file = File::open(path).map_err(|source| IngestError::Io {
        path: path.display().to_string(),
        source,
    })?;
    parse_xml_reader(BufReader::new(file))
}

pub fn parse_xml_str(xml: &str) -> Result<OsmDocument, IngestError> {
    parse_xml_reader(std::io::Cursor::new(xml.as_bytes()))
}

fn parse_xml_reader<R: std::io::BufRead>(reader: R) -> Result<OsmDocument, IngestError> {
    let mut xml = Reader::from_reader(reader);
    xml.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut document = OsmDocument::default();
    let mut scope = Scope::Root;
    let mut way = WayBuild::default();
    let mut relation = RelationBuild::default();
    let mut node_id = 0u64;
    let mut node_lon = 0.0;
    let mut node_lat = 0.0;
    let mut node_signal = false;
    let mut node_mini = false;
    let mut node_junction = false;

    loop {
        let event = xml
            .read_event_into(&mut buffer)
            .map_err(|error| IngestError::Xml(error.to_string()))?;
        match event {
            Event::Start(element) => match local_name(element.name().as_ref()) {
                b"node" => {
                    scope = Scope::Node;
                    let attrs = attributes(&element);
                    node_id = attr_u64(&attrs, "id").unwrap_or(0);
                    node_lon = attr_f64(&attrs, "lon").unwrap_or(0.0);
                    node_lat = attr_f64(&attrs, "lat").unwrap_or(0.0);
                    node_signal = false;
                    node_mini = false;
                    node_junction = false;
                }
                b"way" => {
                    scope = Scope::Way;
                    way = WayBuild::default();
                    way.id = attr_u64(&attributes(&element), "id").unwrap_or(0);
                }
                b"relation" => {
                    scope = Scope::Relation;
                    relation = RelationBuild::default();
                }
                b"nd" if scope == Scope::Way => {
                    if let Some(reference) = attr_u64(&attributes(&element), "ref") {
                        way.nodes.push(reference);
                    }
                }
                b"tag" => apply_tag(
                    scope,
                    &attributes(&element),
                    &mut way,
                    &mut relation,
                    &mut node_signal,
                    &mut node_mini,
                    &mut node_junction,
                ),
                b"member" if scope == Scope::Relation => {
                    let attrs = attributes(&element);
                    relation.members.push(Member {
                        kind: attr_string(&attrs, "type"),
                        reference: attr_u64(&attrs, "ref").unwrap_or(0),
                        role: attr_string(&attrs, "role"),
                    });
                }
                _ => {}
            },
            Event::Empty(element) => match local_name(element.name().as_ref()) {
                b"node" => {
                    let attrs = attributes(&element);
                    if let Some(id) = attr_u64(&attrs, "id") {
                        document.nodes.insert(
                            id,
                            OsmNode {
                                lon: attr_f64(&attrs, "lon").unwrap_or(0.0),
                                lat: attr_f64(&attrs, "lat").unwrap_or(0.0),
                                signal: false,
                                mini_roundabout: false,
                                motorway_junction: false,
                            },
                        );
                    }
                }
                b"nd" if scope == Scope::Way => {
                    if let Some(reference) = attr_u64(&attributes(&element), "ref") {
                        way.nodes.push(reference);
                    }
                }
                b"tag" => apply_tag(
                    scope,
                    &attributes(&element),
                    &mut way,
                    &mut relation,
                    &mut node_signal,
                    &mut node_mini,
                    &mut node_junction,
                ),
                b"member" if scope == Scope::Relation => {
                    let attrs = attributes(&element);
                    relation.members.push(Member {
                        kind: attr_string(&attrs, "type"),
                        reference: attr_u64(&attrs, "ref").unwrap_or(0),
                        role: attr_string(&attrs, "role"),
                    });
                }
                _ => {}
            },
            Event::End(element) => match local_name(element.name().as_ref()) {
                b"node" if scope == Scope::Node => {
                    if node_id != 0 {
                        document.nodes.insert(
                            node_id,
                            OsmNode {
                                lon: node_lon,
                                lat: node_lat,
                                signal: node_signal,
                                mini_roundabout: node_mini,
                                motorway_junction: node_junction,
                            },
                        );
                    }
                    scope = Scope::Root;
                }
                b"way" if scope == Scope::Way => {
                    let finished = std::mem::take(&mut way);
                    if finished.id != 0 && !finished.highway.is_empty() && finished.nodes.len() >= 2
                    {
                        document.ways.push(finished.finish());
                    }
                    scope = Scope::Root;
                }
                b"relation" if scope == Scope::Relation => {
                    let finished = std::mem::take(&mut relation);
                    if let Some(restriction) = finished.finish() {
                        document.restrictions.push(restriction);
                    }
                    scope = Scope::Root;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(document)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Root,
    Node,
    Way,
    Relation,
}

#[derive(Default)]
struct WayBuild {
    id: u64,
    nodes: Vec<u64>,
    tags: FxHashMap<String, String>,
    highway: String,
}

impl WayBuild {
    fn finish(self) -> OsmWay {
        let tag = |key: &str| self.tags.get(key).cloned();
        let flag = |key: &str| {
            self.tags
                .get(key)
                .is_some_and(|value| !matches!(value.as_str(), "no" | "false" | "0"))
        };
        OsmWay {
            id: self.id,
            nodes: self.nodes,
            highway: self.highway,
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
            area: self
                .tags
                .get("area")
                .is_some_and(|value| matches!(value.as_str(), "yes" | "true" | "1")),
        }
    }
}

#[derive(Default)]
struct RelationBuild {
    tags: FxHashMap<String, String>,
    members: Vec<Member>,
}

struct Member {
    kind: String,
    reference: u64,
    role: String,
}

impl RelationBuild {
    fn finish(self) -> Option<OsmRestriction> {
        if self.tags.get("type").map(String::as_str) != Some("restriction") {
            return None;
        }
        let restriction = self.tags.get("restriction")?.clone();
        let from_way = self
            .members
            .iter()
            .find(|member| member.role == "from" && member.kind == "way")?
            .reference;
        let to_way = self
            .members
            .iter()
            .find(|member| member.role == "to" && member.kind == "way")?
            .reference;
        let via_node = self
            .members
            .iter()
            .find(|member| member.role == "via" && member.kind == "node")
            .map(|member| member.reference);
        let via_way = self
            .members
            .iter()
            .find(|member| member.role == "via" && member.kind == "way")
            .map(|member| member.reference);
        if via_node.is_none() && via_way.is_none() {
            return None;
        }
        Some(OsmRestriction {
            from_way,
            to_way,
            via_node,
            via_way,
            restriction,
        })
    }
}

fn apply_tag(
    scope: Scope,
    attrs: &[(String, String)],
    way: &mut WayBuild,
    relation: &mut RelationBuild,
    node_signal: &mut bool,
    node_mini: &mut bool,
    node_junction: &mut bool,
) {
    let Some(key) = attrs
        .iter()
        .find(|(key, _)| key == "k")
        .map(|(_, value)| value.clone())
    else {
        return;
    };
    let Some(value) = attrs
        .iter()
        .find(|(key, _)| key == "v")
        .map(|(_, value)| value.clone())
    else {
        return;
    };
    match scope {
        Scope::Way => {
            if key == "highway" {
                way.highway = value.clone();
            }
            way.tags.insert(key, value);
        }
        Scope::Relation => {
            relation.tags.insert(key, value);
        }
        Scope::Node => match (key.as_str(), value.as_str()) {
            ("highway", "traffic_signals") | ("crossing", "traffic_signals") => *node_signal = true,
            ("highway", "mini_roundabout") => *node_mini = true,
            ("highway", "motorway_junction") => *node_junction = true,
            _ => {}
        },
        Scope::Root => {}
    }
}

fn attributes(element: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    element
        .attributes()
        .flatten()
        .map(|attr| {
            let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
            let value = attr
                .unescape_value()
                .map(|text| text.into_owned())
                .unwrap_or_default();
            (key, value)
        })
        .collect()
}

fn attr_string(attrs: &[(String, String)], name: &str) -> String {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

fn attr_u64(attrs: &[(String, String)], name: &str) -> Option<u64> {
    attr_string(attrs, name).parse().ok()
}

fn attr_f64(attrs: &[(String, String)], name: &str) -> Option<f64> {
    attr_string(attrs, name).parse().ok()
}

fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().position(|byte| *byte == b':') {
        Some(index) => &name[index + 1..],
        None => name,
    }
}
