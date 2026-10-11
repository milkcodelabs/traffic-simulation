use rustc_hash::FxHashMap;

#[derive(Clone, Debug)]
pub struct OsmNode {
    pub lon: f64,
    pub lat: f64,
    pub signal: bool,
    pub mini_roundabout: bool,
    pub motorway_junction: bool,
}

#[derive(Clone, Debug)]
pub struct OsmWay {
    pub id: u64,
    pub nodes: Vec<u64>,
    pub highway: String,
    pub oneway: Option<String>,
    pub lanes: Option<String>,
    pub lanes_forward: Option<String>,
    pub lanes_backward: Option<String>,
    pub maxspeed: Option<String>,
    pub name: Option<String>,
    pub road_ref: Option<String>,
    pub junction: Option<String>,
    pub bridge: bool,
    pub tunnel: bool,
    pub access: Option<String>,
    pub motor_vehicle: Option<String>,
    pub motorcar: Option<String>,
    pub service: Option<String>,
    pub area: bool,
}

#[derive(Clone, Debug)]
pub struct OsmRestriction {
    pub from_way: u64,
    pub to_way: u64,
    pub via_node: Option<u64>,
    pub via_way: Option<u64>,
    pub restriction: String,
}

#[derive(Clone, Debug, Default)]
pub struct OsmDocument {
    pub nodes: FxHashMap<u64, OsmNode>,
    pub ways: Vec<OsmWay>,
    pub restrictions: Vec<OsmRestriction>,
}

pub const SYNTHETIC_NODE_BASE: u64 = 0xF000_0000_0000_0000;

pub fn is_synthetic(osm_id: u64) -> bool {
    osm_id >= SYNTHETIC_NODE_BASE
}
