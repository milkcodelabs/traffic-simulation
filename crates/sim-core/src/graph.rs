use serde::{Deserialize, Serialize};

use crate::cache::FORMAT_VERSION;
use crate::fd::DiagramFamily;
use crate::geo::{endpoint_bearings, polyline_length_m, BBox, LonLat, Xy};
use crate::id::{EdgeId, NodeId, SignalId, ZoneId};
use crate::road::RoadClass;
use crate::signal::SignalController;
use crate::turns::Movement;
use crate::utm::UtmProjection;
use crate::CoreError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Intersection,
    Gateway,
    Roundabout,
    RampSplit,
    Signalized,
    DeadEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeClassHints {
    pub signalized: bool,
    pub mini_roundabout: bool,
    pub on_roundabout: bool,
    pub self_loop: bool,
    pub degree_undirected: u32,
    pub has_link: bool,
    pub has_non_link: bool,
    pub gateway: bool,
}

pub fn classify_node(hints: NodeClassHints) -> NodeKind {
    if hints.signalized {
        NodeKind::Signalized
    } else if hints.mini_roundabout
        || (hints.on_roundabout && (hints.degree_undirected > 2 || hints.self_loop))
    {
        NodeKind::Roundabout
    } else if hints.gateway {
        NodeKind::Gateway
    } else if hints.degree_undirected <= 1 {
        NodeKind::DeadEnd
    } else if hints.has_link && hints.has_non_link {
        NodeKind::RampSplit
    } else {
        NodeKind::Intersection
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub kind: NodeKind,
    pub lonlat: LonLat,
    pub xy: Xy,
    pub elevation_m: Option<f64>,
    pub osm_id: u64,
    pub synthetic: bool,
    pub signal: Option<SignalId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeFlags {
    pub roundabout: bool,
    pub bridge: bool,
    pub tunnel: bool,
    pub destination_only: bool,
    pub dead_end_reversal: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub geometry_xy: Vec<Xy>,
    pub geometry_lonlat: Vec<LonLat>,
    pub class: RoadClass,
    pub lanes: u8,
    pub length_m: f64,
    pub grade_rad: f64,
    pub uphill_grade_rad: f64,
    pub free_flow_speed_mps: f64,
    pub base_free_flow_speed_mps: f64,
    pub critical_speed_mps: f64,
    pub capacity_veh_per_s: f64,
    pub base_capacity_veh_per_s: f64,
    pub jam_density_per_lane: f64,
    pub wave_speed_mps: f64,
    pub storage_max_veh: f64,
    pub osm_way_ids: Vec<u64>,
    pub name: Option<String>,
    pub road_ref: Option<String>,
    pub flags: EdgeFlags,
}

impl Edge {
    pub fn jam_density_veh_per_m(&self) -> f64 {
        self.jam_density_per_lane * f64::from(self.lanes)
    }

    /// Last source way, which is the way that reaches `to` after contraction.
    pub fn arrives_as(&self, way_id: u64) -> bool {
        self.osm_way_ids.last().copied() == Some(way_id)
    }

    /// First source way, which is the way that leaves `from` after contraction.
    pub fn leaves_as(&self, way_id: u64) -> bool {
        self.osm_way_ids.first().copied() == Some(way_id)
    }

    pub fn bearings(&self) -> Option<(f64, f64)> {
        endpoint_bearings(&self.geometry_xy)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub node: NodeId,
    pub from_edge: EdgeId,
    pub to_edge: EdgeId,
    pub movement: Movement,
    pub angle_rad: f64,
    pub allowed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adjacency {
    pub out_offset: Vec<u32>,
    pub out_edges: Vec<EdgeId>,
    pub in_offset: Vec<u32>,
    pub in_edges: Vec<EdgeId>,
}

impl Adjacency {
    pub fn outgoing(&self, node: NodeId) -> &[EdgeId] {
        let start = self.out_offset[node.index()] as usize;
        let end = self.out_offset[node.index() + 1] as usize;
        &self.out_edges[start..end]
    }

    pub fn incoming(&self, node: NodeId) -> &[EdgeId] {
        let start = self.in_offset[node.index()] as usize;
        let end = self.in_offset[node.index() + 1] as usize;
        &self.in_edges[start..end]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZoneKind {
    Gateway,
    Activity,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Zone {
    pub kind: ZoneKind,
    pub nodes: Vec<NodeId>,
    pub centroid: LonLat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStats {
    pub nodes: u32,
    pub edges: u32,
    pub turns: u32,
    pub banned_turns: u32,
    pub signals: u32,
    pub gateways: u32,
    pub zones: u32,
    pub total_length_m: f64,
    pub total_lane_m: f64,
    pub mean_edge_length_m: f64,
    pub contractions: u32,
    pub bbox: BBox,
    pub diagram: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    pub format_version: u32,
    pub preset_id: Option<String>,
    pub bbox: BBox,
    pub projection: UtmProjection,
    pub diagram: DiagramFamily,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub adjacency: Adjacency,
    pub turns: Vec<Turn>,
    pub turn_offset: Vec<u32>,
    pub signals: Vec<SignalController>,
    pub zones: Vec<Zone>,
    pub contractions: u32,
    pub compiled_unix_s: u64,
    pub stats: NetworkStats,
}

impl Network {
    pub fn turns_at(&self, node: NodeId) -> &[Turn] {
        let start = self.turn_offset[node.index()] as usize;
        let end = self.turn_offset[node.index() + 1] as usize;
        &self.turns[start..end]
    }

    pub fn zone_id_of(&self, index: usize) -> ZoneId {
        ZoneId(index as u32)
    }
}

pub struct AssembleInput {
    pub preset_id: Option<String>,
    pub bbox: BBox,
    pub projection: UtmProjection,
    pub diagram: DiagramFamily,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub turns: Vec<Turn>,
    pub signals: Vec<SignalController>,
    pub zones: Vec<Zone>,
    pub contractions: u32,
    pub compiled_unix_s: u64,
}

impl Network {
    pub fn assemble(mut input: AssembleInput) -> Result<Self, CoreError> {
        if input.nodes.is_empty() || input.edges.is_empty() {
            return Err(CoreError::Invariant(
                "a network needs at least one node and one edge".into(),
            ));
        }
        if input.nodes.len() > u32::MAX as usize || input.edges.len() > u32::MAX as usize {
            return Err(CoreError::Invariant("graph exceeds u32 indexing".into()));
        }

        for node in &mut input.nodes {
            node.signal = None;
        }
        for (index, signal) in input.signals.iter_mut().enumerate() {
            signal.id = SignalId(index as u32);
            let node_index = signal.node.index();
            if node_index >= input.nodes.len() {
                return Err(CoreError::Invariant(format!(
                    "signal references missing node {node_index}"
                )));
            }
            input.nodes[node_index].signal = Some(signal.id);
        }

        input.turns.sort_by_key(|turn| turn.node.0);
        let adjacency = build_adjacency(input.nodes.len(), &input.edges)?;
        let turn_offset = build_turn_offset(input.nodes.len(), &input.turns)?;
        let stats = compute_stats(&input);

        let network = Self {
            format_version: FORMAT_VERSION,
            preset_id: input.preset_id,
            bbox: input.bbox,
            projection: input.projection,
            diagram: input.diagram,
            nodes: input.nodes,
            edges: input.edges,
            adjacency,
            turns: input.turns,
            turn_offset,
            signals: input.signals,
            zones: input.zones,
            contractions: input.contractions,
            compiled_unix_s: input.compiled_unix_s,
            stats,
        };
        network.validate()?;
        Ok(network)
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        if self.format_version != FORMAT_VERSION {
            return Err(CoreError::CacheVersion {
                found: self.format_version,
                expected: FORMAT_VERSION,
            });
        }
        if self.nodes.is_empty() || self.edges.is_empty() {
            return Err(CoreError::Invariant("network is empty".into()));
        }
        if self.adjacency.out_offset.len() != self.nodes.len() + 1
            || self.adjacency.in_offset.len() != self.nodes.len() + 1
            || self.turn_offset.len() != self.nodes.len() + 1
        {
            return Err(CoreError::Invariant(
                "adjacency offsets do not match the node count".into(),
            ));
        }
        if *self.adjacency.out_offset.last().unwrap_or(&0) as usize
            != self.adjacency.out_edges.len()
            || *self.adjacency.in_offset.last().unwrap_or(&0) as usize
                != self.adjacency.in_edges.len()
            || *self.turn_offset.last().unwrap_or(&0) as usize != self.turns.len()
        {
            return Err(CoreError::Invariant(
                "adjacency cursor does not match its edge list".into(),
            ));
        }

        for (index, edge) in self.edges.iter().enumerate() {
            if edge.from.index() >= self.nodes.len() || edge.to.index() >= self.nodes.len() {
                return Err(CoreError::Invariant(format!(
                    "edge {index} leaves the node table"
                )));
            }
            if edge.length_m < 1.0
                || edge.lanes == 0
                || edge.free_flow_speed_mps <= 0.0
                || edge.base_free_flow_speed_mps <= 0.0
            {
                return Err(CoreError::Invariant(format!(
                    "edge {index} has a non-physical length, lane count, or speed"
                )));
            }
            if edge.capacity_veh_per_s < 0.0
                || edge.jam_density_per_lane <= 0.0
                || edge.osm_way_ids.is_empty()
            {
                return Err(CoreError::Invariant(format!(
                    "edge {index} has a non-physical capacity or no source way"
                )));
            }
            let expected_storage =
                f64::from(edge.lanes) * edge.jam_density_per_lane * edge.length_m;
            let scale = expected_storage.max(1.0);
            if (edge.storage_max_veh - expected_storage).abs() > 1e-6 * scale {
                return Err(CoreError::Invariant(format!(
                    "edge {index} storage does not match lanes * jam density * length"
                )));
            }
            let geometric = polyline_length_m(&edge.geometry_xy);
            if (geometric - edge.length_m).abs() > 1e-6 * edge.length_m.max(1.0) {
                return Err(CoreError::Invariant(format!(
                    "edge {index} length does not match its polyline"
                )));
            }
            if edge.critical_speed_mps <= 0.0
                || edge.capacity_veh_per_s / edge.critical_speed_mps >= edge.jam_density_veh_per_m()
            {
                return Err(CoreError::Invariant(format!(
                    "edge {index} critical density is not below jam density"
                )));
            }
            let outgoing = self.adjacency.outgoing(edge.from);
            if !outgoing.contains(&EdgeId(index as u32)) {
                return Err(CoreError::Invariant(format!(
                    "edge {index} is missing from the outgoing adjacency"
                )));
            }
        }

        for (index, turn) in self.turns.iter().enumerate() {
            if turn.node.index() >= self.nodes.len()
                || turn.from_edge.index() >= self.edges.len()
                || turn.to_edge.index() >= self.edges.len()
            {
                return Err(CoreError::Invariant(format!(
                    "turn {index} references a missing element"
                )));
            }
            let incoming = &self.edges[turn.from_edge.index()];
            let outgoing = &self.edges[turn.to_edge.index()];
            if incoming.to != turn.node || outgoing.from != turn.node {
                return Err(CoreError::Invariant(format!(
                    "turn {index} is not seated at its node"
                )));
            }
        }

        for (index, signal) in self.signals.iter().enumerate() {
            if signal.id.index() != index || signal.node.index() >= self.nodes.len() {
                return Err(CoreError::Invariant(format!(
                    "signal {index} is not indexed by its node"
                )));
            }
            if self.nodes[signal.node.index()].signal != Some(signal.id) {
                return Err(CoreError::Invariant(format!(
                    "node does not point back at signal {index}"
                )));
            }
            for phase in &signal.phases {
                for edge_id in &phase.incoming {
                    if edge_id.index() >= self.edges.len()
                        || self.edges[edge_id.index()].to != signal.node
                    {
                        return Err(CoreError::Invariant(format!(
                            "signal {index} greens an edge that does not enter its node"
                        )));
                    }
                }
            }
        }

        for zone in &self.zones {
            for node in &zone.nodes {
                if node.index() >= self.nodes.len() {
                    return Err(CoreError::Invariant(
                        "zone references a missing node".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Undirected connectivity of the directed graph. One-way edges still join their endpoints.
    pub fn weak_components(&self) -> ComponentCensus {
        let count = self.nodes.len();
        let mut parent: Vec<u32> = (0..count as u32).collect();
        let mut rank = vec![0u8; count];
        for edge in &self.edges {
            unite(&mut parent, &mut rank, edge.from.0, edge.to.0);
        }
        let mut members = vec![0u32; count];
        let mut length_in = vec![0.0; count];
        let mut touched = vec![false; count];
        for edge in &self.edges {
            let root = find(&mut parent, edge.from.0) as usize;
            length_in[root] += edge.length_m;
            touched[edge.from.index()] = true;
            touched[edge.to.index()] = true;
        }
        for index in 0..count {
            let root = find(&mut parent, index as u32) as usize;
            members[root] += 1;
        }
        let mut components = 0u32;
        let mut isolated_nodes = 0u32;
        let mut largest_nodes = 0u32;
        let mut largest_length_m = 0.0;
        for index in 0..count {
            if parent[index] != index as u32 {
                continue;
            }
            components += 1;
            largest_nodes = largest_nodes.max(members[index]);
            if length_in[index] > largest_length_m {
                largest_length_m = length_in[index];
            }
            if members[index] == 1 && !touched[index] {
                isolated_nodes += 1;
            }
        }
        ComponentCensus {
            components,
            largest_nodes,
            largest_length_m,
            isolated_nodes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComponentCensus {
    pub components: u32,
    pub largest_nodes: u32,
    pub largest_length_m: f64,
    pub isolated_nodes: u32,
}

fn find(parent: &mut [u32], mut index: u32) -> u32 {
    let mut root = index;
    while parent[root as usize] != root {
        root = parent[root as usize];
    }
    while parent[index as usize] != root {
        let next = parent[index as usize];
        parent[index as usize] = root;
        index = next;
    }
    root
}

fn unite(parent: &mut [u32], rank: &mut [u8], left: u32, right: u32) {
    let mut left = find(parent, left);
    let mut right = find(parent, right);
    if left == right {
        return;
    }
    if rank[left as usize] < rank[right as usize] {
        std::mem::swap(&mut left, &mut right);
    }
    parent[right as usize] = left;
    if rank[left as usize] == rank[right as usize] {
        rank[left as usize] = rank[left as usize].saturating_add(1);
    }
}

fn build_adjacency(node_count: usize, edges: &[Edge]) -> Result<Adjacency, CoreError> {
    let mut out_degree = vec![0u32; node_count];
    let mut in_degree = vec![0u32; node_count];
    for edge in edges {
        let from = edge.from.index();
        let to = edge.to.index();
        if from >= node_count || to >= node_count {
            return Err(CoreError::Invariant(
                "edge endpoint is outside the node table".into(),
            ));
        }
        out_degree[from] = out_degree[from].saturating_add(1);
        in_degree[to] = in_degree[to].saturating_add(1);
    }
    let mut out_offset = Vec::with_capacity(node_count + 1);
    let mut in_offset = Vec::with_capacity(node_count + 1);
    out_offset.push(0);
    in_offset.push(0);
    for index in 0..node_count {
        out_offset.push(out_offset[index] + out_degree[index]);
        in_offset.push(in_offset[index] + in_degree[index]);
    }
    let mut out_edges = vec![EdgeId::INVALID; out_offset[node_count] as usize];
    let mut in_edges = vec![EdgeId::INVALID; in_offset[node_count] as usize];
    let mut out_cursor = out_offset.clone();
    let mut in_cursor = in_offset.clone();
    for (index, edge) in edges.iter().enumerate() {
        let id = EdgeId(index as u32);
        let from = edge.from.index();
        let to = edge.to.index();
        out_edges[out_cursor[from] as usize] = id;
        out_cursor[from] += 1;
        in_edges[in_cursor[to] as usize] = id;
        in_cursor[to] += 1;
    }
    Ok(Adjacency {
        out_offset,
        out_edges,
        in_offset,
        in_edges,
    })
}

fn build_turn_offset(node_count: usize, turns: &[Turn]) -> Result<Vec<u32>, CoreError> {
    let mut counts = vec![0u32; node_count];
    for turn in turns {
        let node = turn.node.index();
        if node >= node_count {
            return Err(CoreError::Invariant(
                "turn references a missing node".into(),
            ));
        }
        counts[node] = counts[node].saturating_add(1);
    }
    let mut offset = Vec::with_capacity(node_count + 1);
    offset.push(0);
    for count in counts {
        let last = *offset.last().unwrap_or(&0);
        offset.push(last + count);
    }
    Ok(offset)
}

fn compute_stats(input: &AssembleInput) -> NetworkStats {
    let total_length_m = input.edges.iter().map(|edge| edge.length_m).sum::<f64>();
    let total_lane_m = input
        .edges
        .iter()
        .map(|edge| edge.length_m * f64::from(edge.lanes))
        .sum::<f64>();
    NetworkStats {
        nodes: input.nodes.len() as u32,
        edges: input.edges.len() as u32,
        turns: input.turns.len() as u32,
        banned_turns: input.turns.iter().filter(|turn| !turn.allowed).count() as u32,
        signals: input.signals.len() as u32,
        gateways: input
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Gateway)
            .count() as u32,
        zones: input.zones.len() as u32,
        total_length_m,
        total_lane_m,
        mean_edge_length_m: total_length_m / input.edges.len() as f64,
        contractions: input.contractions,
        bbox: input.bbox,
        diagram: input.diagram.name().to_string(),
    }
}
