use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rustc_hash::{FxHashMap, FxHashSet};
use sim_core::{
    boundary_distance_m, classify_movement, classify_node, plan_signal, polyline_length_m,
    realize_edge, Approach, AssembleInput, BBox, CoreError, DiagramFamily, Edge, EdgeDraft,
    EdgeFlags, EdgeId, ElevationSampler, LonLat, Movement, Network, Node, NodeClassHints, NodeId,
    NodeKind, RealizeError, RoadClass, Turn, UtmProjection, Zone, ZoneKind,
    DEFAULT_HEAVY_VEHICLE_SHARE,
};

use crate::clip::{clip_chain, ShapePoint};
use crate::error::IngestError;
use crate::heuristics::{lanes_for, resolve_way, ResolvedWay, Travel};
use crate::model::{
    is_synthetic, OsmDocument, OsmNode, OsmRestriction, OsmWay, SYNTHETIC_NODE_BASE,
};

#[derive(Clone, Debug)]
pub struct CompileConfig {
    pub bbox: BBox,
    pub diagram: DiagramFamily,
    pub heavy_vehicle_share: f64,
    pub activity_zone_m: f64,
    pub gateway_margin_m: f64,
    pub preset_id: Option<String>,
}

impl CompileConfig {
    pub fn new(bbox: BBox) -> Self {
        Self {
            bbox,
            diagram: DiagramFamily::Triangular,
            heavy_vehicle_share: DEFAULT_HEAVY_VEHICLE_SHARE,
            activity_zone_m: 1_000.0,
            gateway_margin_m: 40.0,
            preset_id: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CompileWarnings {
    pub dropped_ways: u32,
    pub dropped_short_edges: u32,
    pub unreadable_speeds: u32,
    pub clipped_ways: u32,
    pub restrictions_applied: u32,
    pub restrictions_unmatched: u32,
    pub dead_end_reversals: u32,
}

impl CompileWarnings {
    pub fn messages(&self) -> Vec<String> {
        let mut messages = Vec::new();
        if self.dropped_ways > 0 {
            messages.push(format!(
                "{} OSM ways were dropped by the driveable-road filter or fell outside the box",
                self.dropped_ways
            ));
        }
        if self.dropped_short_edges > 0 {
            messages.push(format!(
                "{} edges shorter than 1 m were dropped",
                self.dropped_short_edges
            ));
        }
        if self.unreadable_speeds > 0 {
            messages.push(format!(
                "{} maxspeed tags were unreadable and used the European urban fallback",
                self.unreadable_speeds
            ));
        }
        if self.clipped_ways > 0 {
            messages.push(format!(
                "{} ways were clipped to the bounding box",
                self.clipped_ways
            ));
        }
        if self.restrictions_unmatched > 0 {
            messages.push(format!(
                "{} turn restrictions did not match a surviving movement",
                self.restrictions_unmatched
            ));
        }
        if self.dead_end_reversals > 0 {
            messages.push(format!(
                "{} dead-end reversal edges were added so arriving vehicles can leave",
                self.dead_end_reversals
            ));
        }
        messages
    }
}

pub struct CompileOutput {
    pub network: Network,
    pub warnings: CompileWarnings,
    /// Polyline length of directed pieces before degree-2 contraction, in metres.
    pub length_before_contraction_m: f64,
    /// Polyline length after contraction and before dead-end reversals are added.
    pub length_after_contraction_m: f64,
}

pub fn compile_document(
    document: &OsmDocument,
    config: &CompileConfig,
    elevation: &dyn ElevationSampler,
) -> Result<CompileOutput, IngestError> {
    let bbox = config.bbox.validate()?;
    let projection = UtmProjection::for_bbox(bbox);
    let mut warnings = CompileWarnings::default();
    let mut nodes = document.nodes.clone();
    let mut next_synthetic = SYNTHETIC_NODE_BASE;
    let chains = build_chains(
        document,
        bbox,
        &mut nodes,
        &mut next_synthetic,
        &mut warnings,
    );
    let raw = emit_raw(document, &chains, &nodes);
    let length_before_contraction_m = raw_polyline_length_m(&raw, &nodes, projection)?;
    let (raw, contractions) = contract(raw, &nodes);
    let length_after_contraction_m = raw_polyline_length_m(&raw, &nodes, projection)?;
    let contraction_drift_m = (length_before_contraction_m - length_after_contraction_m).abs();
    if contraction_drift_m > 1e-4 {
        return Err(CoreError::Invariant(format!(
            "degree-2 contraction changed total polyline length by {contraction_drift_m} m"
        ))
        .into());
    }
    let (mut edges, mut node_order) =
        realize_raw(&raw, &nodes, projection, elevation, config, &mut warnings)?;
    if edges.is_empty() {
        return Err(IngestError::Empty);
    }
    compact(&mut node_order, &mut edges);
    let gateway_osm = gateway_nodes(
        &node_order,
        &edges,
        &nodes,
        bbox,
        projection,
        config.gateway_margin_m,
    );
    let reversals = dead_end_reversals(&edges, &node_order, &gateway_osm, elevation, config)?;
    warnings.dead_end_reversals = reversals.len() as u32;
    edges.extend(reversals);

    let mut turns = build_turns(&edges, &node_order, &gateway_osm);
    apply_restrictions(document, &node_order, &edges, &mut turns, &mut warnings);
    let signals = build_signals(&node_order, &nodes, &edges);
    let graph_nodes = build_nodes(
        &node_order,
        &edges,
        &nodes,
        &gateway_osm,
        projection,
        elevation,
    );
    let zones = build_zones(&graph_nodes, projection, bbox, config.activity_zone_m);
    let compiled_unix_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);

    let network = Network::assemble(AssembleInput {
        preset_id: config.preset_id.clone(),
        bbox,
        projection,
        diagram: config.diagram,
        nodes: graph_nodes,
        edges,
        turns,
        signals,
        zones,
        contractions,
        compiled_unix_s,
    })?;
    Ok(CompileOutput {
        network,
        warnings,
        length_before_contraction_m,
        length_after_contraction_m,
    })
}

fn raw_polyline_length_m(
    edges: &[RawEdge],
    nodes: &FxHashMap<u64, OsmNode>,
    projection: UtmProjection,
) -> Result<f64, IngestError> {
    let mut total = 0.0;
    for edge in edges {
        let mut xy = Vec::with_capacity(edge.nodes.len());
        for id in &edge.nodes {
            let node = nodes.get(id).ok_or_else(|| {
                IngestError::Core(CoreError::Invariant(format!(
                    "directed piece references missing node {id}"
                )))
            })?;
            xy.push(projection.forward(LonLat {
                lon_deg: node.lon,
                lat_deg: node.lat,
            }));
        }
        total += polyline_length_m(&xy);
    }
    Ok(total)
}

struct Chain {
    way_index: usize,
    points: Vec<ShapePoint>,
    resolved: ResolvedWay,
}

fn build_chains(
    document: &OsmDocument,
    bbox: BBox,
    nodes: &mut FxHashMap<u64, OsmNode>,
    next_synthetic: &mut u64,
    warnings: &mut CompileWarnings,
) -> Vec<Chain> {
    let mut chains = Vec::new();
    for (way_index, way) in document.ways.iter().enumerate() {
        let Some(resolved) = resolve_way(way) else {
            warnings.dropped_ways += 1;
            continue;
        };
        if resolved.speed_warning {
            warnings.unreadable_speeds += 1;
        }
        let mut shape = Vec::with_capacity(way.nodes.len());
        let mut missing = false;
        for id in &way.nodes {
            let Some(node) = nodes.get(id) else {
                missing = true;
                break;
            };
            shape.push(ShapePoint {
                osm_id: *id,
                lonlat: LonLat {
                    lon_deg: node.lon,
                    lat_deg: node.lat,
                },
            });
        }
        if missing {
            warnings.dropped_ways += 1;
            continue;
        }
        let parts = clip_chain(&shape, bbox, nodes, next_synthetic);
        if parts.is_empty() {
            warnings.dropped_ways += 1;
            continue;
        }
        let clipped = parts.len() != 1
            || parts[0].len() != shape.len()
            || parts
                .iter()
                .flatten()
                .any(|point| is_synthetic(point.osm_id));
        if clipped {
            warnings.clipped_ways += 1;
        }
        for points in parts {
            chains.push(Chain {
                way_index,
                points,
                resolved: resolved.clone(),
            });
        }
    }
    chains
}

#[derive(Clone)]
struct RawEdge {
    from: u64,
    to: u64,
    nodes: Vec<u64>,
    class: RoadClass,
    lanes: u8,
    speed_mps: f64,
    flags: EdgeFlags,
    name: Option<String>,
    road_ref: Option<String>,
    osm_way_ids: Vec<u64>,
}

fn emit_raw(
    document: &OsmDocument,
    chains: &[Chain],
    nodes: &FxHashMap<u64, OsmNode>,
) -> Vec<RawEdge> {
    let vertices = vertex_set(chains, nodes);
    let mut raw = Vec::new();
    for chain in chains {
        let way = &document.ways[chain.way_index];
        let oneway = chain.resolved.directions.len() == 1;
        for piece in split_at_vertices(&chain.points, |id| vertices.contains(&id)) {
            for travel in &chain.resolved.directions {
                if let Some(edge) = directed_piece(way, &chain.resolved, &piece, *travel, oneway) {
                    raw.push(edge);
                }
            }
        }
    }
    raw
}

fn vertex_set(chains: &[Chain], nodes: &FxHashMap<u64, OsmNode>) -> FxHashSet<u64> {
    let mut visits: FxHashMap<u64, u32> = FxHashMap::default();
    let mut endpoints = FxHashSet::default();
    for chain in chains {
        for (index, point) in chain.points.iter().enumerate() {
            *visits.entry(point.osm_id).or_default() += 1;
            if index == 0 || index + 1 == chain.points.len() {
                endpoints.insert(point.osm_id);
            }
        }
    }
    visits
        .into_iter()
        .filter(|(id, count)| {
            endpoints.contains(id)
                || *count >= 2
                || is_synthetic(*id)
                || nodes.get(id).is_some_and(|node| {
                    node.signal || node.mini_roundabout || node.motorway_junction
                })
        })
        .map(|(id, _)| id)
        .collect()
}

fn split_at_vertices(
    points: &[ShapePoint],
    is_vertex: impl Fn(u64) -> bool,
) -> Vec<Vec<ShapePoint>> {
    let mut parts = Vec::new();
    let mut current = Vec::new();
    for (index, point) in points.iter().enumerate() {
        current.push(point.clone());
        let cut = index > 0 && is_vertex(point.osm_id);
        if !cut || current.len() < 2 {
            continue;
        }
        let degenerate = current.len() == 2 && current[0].osm_id == point.osm_id;
        if degenerate {
            continue;
        }
        parts.push(std::mem::take(&mut current));
        current.push(point.clone());
    }
    parts
}

fn directed_piece(
    way: &OsmWay,
    resolved: &ResolvedWay,
    piece: &[ShapePoint],
    travel: Travel,
    oneway: bool,
) -> Option<RawEdge> {
    let mut ids: Vec<u64> = piece.iter().map(|point| point.osm_id).collect();
    if travel == Travel::Backward {
        ids.reverse();
    }
    ids.dedup();
    if ids.len() < 2 {
        return None;
    }
    let from = ids[0];
    let to = *ids.last()?;
    Some(RawEdge {
        from,
        to,
        nodes: ids,
        class: resolved.class,
        lanes: lanes_for(way, resolved.class, travel, oneway),
        speed_mps: resolved.speed_mps,
        flags: EdgeFlags {
            roundabout: resolved.roundabout,
            bridge: resolved.bridge,
            tunnel: resolved.tunnel,
            destination_only: resolved.destination_only,
            dead_end_reversal: false,
        },
        name: way.name.clone(),
        road_ref: way.road_ref.clone(),
        osm_way_ids: vec![way.id],
    })
}

fn contract(edges: Vec<RawEdge>, nodes: &FxHashMap<u64, OsmNode>) -> (Vec<RawEdge>, u32) {
    let mut outgoing: FxHashMap<u64, Vec<usize>> = FxHashMap::default();
    let mut incoming: FxHashMap<u64, Vec<usize>> = FxHashMap::default();
    for (index, edge) in edges.iter().enumerate() {
        outgoing.entry(edge.from).or_default().push(index);
        incoming.entry(edge.to).or_default().push(index);
    }
    let contractable = |id: u64| {
        let preserved = is_synthetic(id)
            || nodes
                .get(&id)
                .is_some_and(|node| node.signal || node.mini_roundabout || node.motorway_junction);
        !preserved
            && incoming.get(&id).is_some_and(|items| items.len() == 1)
            && outgoing.get(&id).is_some_and(|items| items.len() == 1)
    };
    let compatible = |left: &RawEdge, right: &RawEdge| {
        left.class == right.class
            && left.lanes == right.lanes
            && (left.speed_mps - right.speed_mps).abs() <= 0.1
            && left.flags == right.flags
    };

    let mut consumed = vec![false; edges.len()];
    let mut merged = Vec::new();
    let mut contractions = 0u32;
    for start in 0..edges.len() {
        if consumed[start] || contractable(edges[start].from) {
            continue;
        }
        consumed[start] = true;
        let mut edge = edges[start].clone();
        loop {
            let node = edge.to;
            if !contractable(node) {
                break;
            }
            let next_index = outgoing[&node][0];
            if consumed[next_index] || !compatible(&edge, &edges[next_index]) {
                break;
            }
            consumed[next_index] = true;
            let next = &edges[next_index];
            edge.nodes.extend(next.nodes.iter().skip(1).copied());
            edge.to = next.to;
            edge.osm_way_ids.extend(next.osm_way_ids.iter().copied());
            contractions += 1;
        }
        merged.push(edge);
    }
    for (index, edge) in edges.into_iter().enumerate() {
        if !consumed[index] {
            merged.push(edge);
        }
    }
    (merged, contractions)
}

fn realize_raw(
    raw: &[RawEdge],
    nodes: &FxHashMap<u64, OsmNode>,
    projection: UtmProjection,
    elevation: &dyn ElevationSampler,
    config: &CompileConfig,
    warnings: &mut CompileWarnings,
) -> Result<(Vec<Edge>, Vec<u64>), IngestError> {
    let mut order = Vec::new();
    let mut index_of: FxHashMap<u64, NodeId> = FxHashMap::default();
    for edge in raw {
        for endpoint in [edge.from, edge.to] {
            if let std::collections::hash_map::Entry::Vacant(slot) = index_of.entry(endpoint) {
                slot.insert(NodeId(order.len() as u32));
                order.push(endpoint);
            }
        }
    }
    let mut edges = Vec::new();
    for edge in raw {
        match realize_one(edge, &index_of, nodes, projection, elevation, config, false) {
            Ok(realized) => edges.push(realized),
            Err(RealizeError::Short { .. }) => warnings.dropped_short_edges += 1,
            Err(error) => return Err(IngestError::Core(error.into())),
        }
    }
    Ok((edges, order))
}

fn realize_one(
    edge: &RawEdge,
    index_of: &FxHashMap<u64, NodeId>,
    nodes: &FxHashMap<u64, OsmNode>,
    projection: UtmProjection,
    elevation: &dyn ElevationSampler,
    config: &CompileConfig,
    reversal: bool,
) -> Result<Edge, RealizeError> {
    let mut geometry_lonlat = Vec::with_capacity(edge.nodes.len());
    let mut geometry_xy = Vec::with_capacity(edge.nodes.len());
    let mut elevations = Vec::with_capacity(edge.nodes.len());
    for id in &edge.nodes {
        let Some(node) = nodes.get(id) else {
            return Err(RealizeError::Geometry);
        };
        let lonlat = LonLat {
            lon_deg: node.lon,
            lat_deg: node.lat,
        };
        geometry_xy.push(projection.forward(lonlat));
        elevations.push(elevation.sample(lonlat.lon_deg, lonlat.lat_deg));
        geometry_lonlat.push(lonlat);
    }
    let mut flags = edge.flags;
    flags.dead_end_reversal = reversal || edge.flags.dead_end_reversal;
    realize_edge(EdgeDraft {
        from: index_of[&edge.from],
        to: index_of[&edge.to],
        geometry_xy,
        geometry_lonlat,
        elevations_m: elevations,
        ignore_grade: edge.flags.bridge || edge.flags.tunnel,
        class: edge.class,
        lanes: edge.lanes,
        posted_speed_mps: edge.speed_mps,
        osm_way_ids: edge.osm_way_ids.clone(),
        name: edge.name.clone(),
        road_ref: edge.road_ref.clone(),
        flags,
        diagram: config.diagram,
        heavy_vehicle_share: config.heavy_vehicle_share,
    })
}

fn compact(order: &mut Vec<u64>, edges: &mut [Edge]) {
    let mut used = vec![false; order.len()];
    for edge in edges.iter() {
        used[edge.from.index()] = true;
        used[edge.to.index()] = true;
    }
    if used.iter().all(|flag| *flag) {
        return;
    }
    let mut remap = vec![NodeId::INVALID; order.len()];
    let mut next = Vec::new();
    for (index, flag) in used.iter().enumerate() {
        if *flag {
            remap[index] = NodeId(next.len() as u32);
            next.push(order[index]);
        }
    }
    for edge in edges.iter_mut() {
        edge.from = remap[edge.from.index()];
        edge.to = remap[edge.to.index()];
    }
    *order = next;
}

fn gateway_nodes(
    order: &[u64],
    edges: &[Edge],
    nodes: &FxHashMap<u64, OsmNode>,
    bbox: BBox,
    projection: UtmProjection,
    margin_m: f64,
) -> FxHashSet<u64> {
    let mut neighbors: FxHashMap<u64, FxHashSet<u64>> = FxHashMap::default();
    for edge in edges {
        let from = order[edge.from.index()];
        let to = order[edge.to.index()];
        if from != to {
            neighbors.entry(from).or_default().insert(to);
            neighbors.entry(to).or_default().insert(from);
        }
    }
    let mut gateways = FxHashSet::default();
    for osm_id in order {
        if is_synthetic(*osm_id) {
            gateways.insert(*osm_id);
            continue;
        }
        let degree = neighbors.get(osm_id).map(|set| set.len()).unwrap_or(0);
        let Some(node) = nodes.get(osm_id) else {
            continue;
        };
        let distance = boundary_distance_m(
            bbox,
            LonLat {
                lon_deg: node.lon,
                lat_deg: node.lat,
            },
            projection,
        );
        if degree <= 1 && distance <= margin_m {
            gateways.insert(*osm_id);
        }
    }
    gateways
}

fn dead_end_reversals(
    edges: &[Edge],
    order: &[u64],
    gateways: &FxHashSet<u64>,
    elevation: &dyn ElevationSampler,
    config: &CompileConfig,
) -> Result<Vec<Edge>, IngestError> {
    let mut out_count = vec![0u32; order.len()];
    for edge in edges {
        out_count[edge.from.index()] += 1;
    }
    let mut reversals = Vec::new();
    for edge in edges {
        let sink = order[edge.to.index()];
        if gateways.contains(&sink) || out_count[edge.to.index()] > 0 {
            continue;
        }
        match reverse_edge(edge, elevation, config) {
            Ok(realized) => reversals.push(realized),
            Err(RealizeError::Short { .. }) => {}
            Err(error) => return Err(IngestError::Core(error.into())),
        }
    }
    Ok(reversals)
}

fn reverse_edge(
    edge: &Edge,
    elevation: &dyn ElevationSampler,
    config: &CompileConfig,
) -> Result<Edge, RealizeError> {
    let mut geometry_lonlat = edge.geometry_lonlat.clone();
    geometry_lonlat.reverse();
    let mut geometry_xy = edge.geometry_xy.clone();
    geometry_xy.reverse();
    let elevations_m = geometry_lonlat
        .iter()
        .map(|point| elevation.sample(point.lon_deg, point.lat_deg))
        .collect::<Vec<_>>();
    let mut flags = edge.flags;
    flags.dead_end_reversal = true;
    let mut osm_way_ids = edge.osm_way_ids.clone();
    osm_way_ids.reverse();
    realize_edge(EdgeDraft {
        from: edge.to,
        to: edge.from,
        geometry_xy,
        geometry_lonlat,
        elevations_m,
        ignore_grade: flags.bridge || flags.tunnel,
        class: edge.class,
        lanes: edge.lanes,
        posted_speed_mps: edge.base_free_flow_speed_mps,
        osm_way_ids,
        name: edge.name.clone(),
        road_ref: edge.road_ref.clone(),
        flags,
        diagram: config.diagram,
        heavy_vehicle_share: config.heavy_vehicle_share,
    })
}

fn build_turns(edges: &[Edge], order: &[u64], gateways: &FxHashSet<u64>) -> Vec<Turn> {
    let mut incoming: Vec<Vec<EdgeId>> = vec![Vec::new(); order.len()];
    let mut outgoing: Vec<Vec<EdgeId>> = vec![Vec::new(); order.len()];
    for (index, edge) in edges.iter().enumerate() {
        outgoing[edge.from.index()].push(EdgeId(index as u32));
        incoming[edge.to.index()].push(EdgeId(index as u32));
    }
    let mut neighbors: Vec<FxHashSet<usize>> = vec![FxHashSet::default(); order.len()];
    for edge in edges {
        if edge.from != edge.to {
            neighbors[edge.from.index()].insert(edge.to.index());
            neighbors[edge.to.index()].insert(edge.from.index());
        }
    }
    let mut turns = Vec::new();
    for node_index in 0..order.len() {
        let node = NodeId(node_index as u32);
        let allow_uturn =
            !gateways.contains(&order[node_index]) && neighbors[node_index].len() <= 1;
        for from_edge in &incoming[node_index] {
            let Some((_, arrival)) = edges[from_edge.index()].bearings() else {
                continue;
            };
            for to_edge in &outgoing[node_index] {
                if from_edge == to_edge {
                    continue;
                }
                let Some((departure, _)) = edges[to_edge.index()].bearings() else {
                    continue;
                };
                let (movement, angle_rad) = classify_movement(sim_core::Arrival {
                    incoming_bearing_rad: arrival,
                    outgoing_bearing_rad: departure,
                });
                let allowed = !matches!(movement, Movement::UTurn) || allow_uturn;
                turns.push(Turn {
                    node,
                    from_edge: *from_edge,
                    to_edge: *to_edge,
                    movement,
                    angle_rad,
                    allowed,
                });
            }
        }
    }
    turns
}

fn apply_restrictions(
    document: &OsmDocument,
    order: &[u64],
    edges: &[Edge],
    turns: &mut [Turn],
    warnings: &mut CompileWarnings,
) {
    let osm_to_node: FxHashMap<u64, NodeId> = order
        .iter()
        .enumerate()
        .map(|(index, osm_id)| (*osm_id, NodeId(index as u32)))
        .collect();
    let way_nodes = document
        .ways
        .iter()
        .map(|way| (way.id, way.nodes.iter().copied().collect::<FxHashSet<_>>()))
        .collect::<FxHashMap<_, _>>();

    for restriction in &document.restrictions {
        let via = via_nodes(restriction, &osm_to_node, &way_nodes);
        let Some(action) = restriction_action(&restriction.restriction) else {
            warnings.restrictions_unmatched += 1;
            continue;
        };
        if via.is_empty() {
            warnings.restrictions_unmatched += 1;
            continue;
        }
        let matched = match action {
            RestrictionAction::Forbid(filter) => {
                forbid_pairs(restriction, &via, filter, edges, turns)
            }
            RestrictionAction::Only(filter) => keep_only(restriction, &via, filter, edges, turns),
        };
        if matched {
            warnings.restrictions_applied += 1;
        } else {
            warnings.restrictions_unmatched += 1;
        }
    }
}

fn via_nodes(
    restriction: &OsmRestriction,
    osm_to_node: &FxHashMap<u64, NodeId>,
    way_nodes: &FxHashMap<u64, FxHashSet<u64>>,
) -> FxHashSet<NodeId> {
    let mut via = FxHashSet::default();
    if let Some(node) = restriction
        .via_node
        .and_then(|id| osm_to_node.get(&id).copied())
    {
        via.insert(node);
    }
    if let Some(way) = restriction.via_way.and_then(|id| way_nodes.get(&id)) {
        for osm_id in way {
            if let Some(node) = osm_to_node.get(osm_id) {
                via.insert(*node);
            }
        }
    }
    via
}

#[derive(Clone, Copy)]
enum TurnFilter {
    Left,
    Right,
    Through,
    UTurn,
    Any,
}

enum RestrictionAction {
    Forbid(TurnFilter),
    Only(TurnFilter),
}

fn restriction_action(name: &str) -> Option<RestrictionAction> {
    match name {
        "no_left_turn" => Some(RestrictionAction::Forbid(TurnFilter::Left)),
        "no_right_turn" => Some(RestrictionAction::Forbid(TurnFilter::Right)),
        "no_straight_on" => Some(RestrictionAction::Forbid(TurnFilter::Through)),
        "no_u_turn" => Some(RestrictionAction::Forbid(TurnFilter::UTurn)),
        "no_entry" => Some(RestrictionAction::Forbid(TurnFilter::Any)),
        "only_left_turn" => Some(RestrictionAction::Only(TurnFilter::Left)),
        "only_right_turn" => Some(RestrictionAction::Only(TurnFilter::Right)),
        "only_straight_on" => Some(RestrictionAction::Only(TurnFilter::Through)),
        "only_u_turn" => Some(RestrictionAction::Only(TurnFilter::UTurn)),
        _ => None,
    }
}

fn filter_matches(filter: TurnFilter, movement: Movement) -> bool {
    match filter {
        TurnFilter::Any => true,
        TurnFilter::Left => movement == Movement::Left,
        TurnFilter::Right => movement == Movement::Right,
        TurnFilter::Through => movement == Movement::Through,
        TurnFilter::UTurn => movement == Movement::UTurn,
    }
}

fn forbid_pairs(
    restriction: &OsmRestriction,
    via: &FxHashSet<NodeId>,
    filter: TurnFilter,
    edges: &[Edge],
    turns: &mut [Turn],
) -> bool {
    let mut matched = false;
    for turn in turns.iter_mut() {
        if !via.contains(&turn.node) {
            continue;
        }
        let from = &edges[turn.from_edge.index()];
        let to = &edges[turn.to_edge.index()];
        if from.arrives_as(restriction.from_way)
            && to.leaves_as(restriction.to_way)
            && filter_matches(filter, turn.movement)
        {
            turn.allowed = false;
            matched = true;
        }
    }
    matched
}

fn keep_only(
    restriction: &OsmRestriction,
    via: &FxHashSet<NodeId>,
    filter: TurnFilter,
    edges: &[Edge],
    turns: &mut [Turn],
) -> bool {
    let mut approaches = FxHashSet::default();
    for turn in turns.iter() {
        if via.contains(&turn.node)
            && edges[turn.from_edge.index()].arrives_as(restriction.from_way)
        {
            approaches.insert((turn.node.0, turn.from_edge.0));
        }
    }
    let mut matched = false;
    for (node, from_edge) in approaches {
        let mut onto_way = Vec::new();
        let mut keepers = Vec::new();
        for (index, turn) in turns.iter().enumerate() {
            if turn.node.0 != node || turn.from_edge.0 != from_edge {
                continue;
            }
            if edges[turn.to_edge.index()].leaves_as(restriction.to_way) {
                onto_way.push(index);
                if filter_matches(filter, turn.movement) {
                    keepers.push(index);
                }
            }
        }
        if keepers.is_empty() {
            keepers = onto_way;
        }
        if keepers.is_empty() {
            continue;
        }
        matched = true;
        let keep = keepers.into_iter().collect::<FxHashSet<_>>();
        for (index, turn) in turns.iter_mut().enumerate() {
            if turn.node.0 != node || turn.from_edge.0 != from_edge {
                continue;
            }
            if keep.contains(&index) {
                if matches!(filter, TurnFilter::UTurn) {
                    turn.allowed = true;
                }
            } else {
                turn.allowed = false;
            }
        }
    }
    matched
}

fn build_signals(
    order: &[u64],
    nodes: &FxHashMap<u64, OsmNode>,
    edges: &[Edge],
) -> Vec<sim_core::SignalController> {
    let mut incoming: Vec<Vec<EdgeId>> = vec![Vec::new(); order.len()];
    for (index, edge) in edges.iter().enumerate() {
        incoming[edge.to.index()].push(EdgeId(index as u32));
    }
    let mut signals = Vec::new();
    for (index, osm_id) in order.iter().enumerate() {
        if !nodes.get(osm_id).is_some_and(|node| node.signal) {
            continue;
        }
        let approaches = incoming[index]
            .iter()
            .filter_map(|edge_id| {
                let edge = &edges[edge_id.index()];
                let (_, arrival) = edge.bearings()?;
                Some(Approach {
                    edge: *edge_id,
                    bearing_rad: arrival,
                    lanes: edge.lanes,
                    rank: edge.class.rank(),
                })
            })
            .collect::<Vec<_>>();
        signals.push(plan_signal(
            NodeId(index as u32),
            sim_core::SignalId(signals.len() as u32),
            &approaches,
        ));
    }
    signals
}

fn build_nodes(
    order: &[u64],
    edges: &[Edge],
    osm_nodes: &FxHashMap<u64, OsmNode>,
    gateways: &FxHashSet<u64>,
    projection: UtmProjection,
    elevation: &dyn ElevationSampler,
) -> Vec<Node> {
    let mut neighbors: Vec<FxHashSet<usize>> = vec![FxHashSet::default(); order.len()];
    let mut incident: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    for (index, edge) in edges.iter().enumerate() {
        incident[edge.from.index()].push(index);
        incident[edge.to.index()].push(index);
        if edge.from != edge.to {
            neighbors[edge.from.index()].insert(edge.to.index());
            neighbors[edge.to.index()].insert(edge.from.index());
        }
    }
    order
        .iter()
        .enumerate()
        .map(|(index, osm_id)| {
            let source = osm_nodes.get(osm_id);
            let lonlat = source
                .map(|node| LonLat {
                    lon_deg: node.lon,
                    lat_deg: node.lat,
                })
                .unwrap_or(LonLat {
                    lon_deg: 0.0,
                    lat_deg: 0.0,
                });
            let synthetic = is_synthetic(*osm_id);
            let on_roundabout = incident[index]
                .iter()
                .any(|edge| edges[*edge].flags.roundabout);
            let self_loop = edges
                .iter()
                .any(|edge| edge.from.index() == index && edge.to.index() == index);
            let has_link = incident[index]
                .iter()
                .any(|edge| edges[*edge].class.is_link());
            let has_non_link = incident[index]
                .iter()
                .any(|edge| !edges[*edge].class.is_link());
            let kind = classify_node(NodeClassHints {
                signalized: source.is_some_and(|node| node.signal),
                mini_roundabout: source.is_some_and(|node| node.mini_roundabout),
                on_roundabout,
                self_loop,
                degree_undirected: neighbors[index].len() as u32,
                has_link,
                has_non_link,
                gateway: gateways.contains(osm_id),
            });
            Node {
                kind,
                lonlat,
                xy: projection.forward(lonlat),
                elevation_m: elevation.sample(lonlat.lon_deg, lonlat.lat_deg),
                osm_id: if synthetic { 0 } else { *osm_id },
                synthetic,
                signal: None,
            }
        })
        .collect()
}

fn build_zones(nodes: &[Node], projection: UtmProjection, bbox: BBox, cell_m: f64) -> Vec<Zone> {
    let mut zones = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.kind == NodeKind::Gateway {
            zones.push(Zone {
                kind: ZoneKind::Gateway,
                nodes: vec![NodeId(index as u32)],
                centroid: node.lonlat,
            });
        }
    }
    let origin = projection.forward(LonLat {
        lon_deg: bbox.min_lon,
        lat_deg: bbox.min_lat,
    });
    let mut cells: BTreeMap<(i64, i64), Vec<NodeId>> = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.kind == NodeKind::Gateway {
            continue;
        }
        let column = ((node.xy.x_m - origin.x_m) / cell_m).floor() as i64;
        let row = ((node.xy.y_m - origin.y_m) / cell_m).floor() as i64;
        cells
            .entry((column, row))
            .or_default()
            .push(NodeId(index as u32));
    }
    for members in cells.into_values() {
        let centroid = LonLat {
            lon_deg: members
                .iter()
                .map(|id| nodes[id.index()].lonlat.lon_deg)
                .sum::<f64>()
                / members.len() as f64,
            lat_deg: members
                .iter()
                .map(|id| nodes[id.index()].lonlat.lat_deg)
                .sum::<f64>()
                / members.len() as f64,
        };
        zones.push(Zone {
            kind: ZoneKind::Activity,
            nodes: members,
            centroid,
        });
    }
    zones
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{store_network, FlatElevation, Movement, NodeKind};
    use std::path::PathBuf;

    fn wide_box() -> BBox {
        BBox {
            min_lat: 40.50,
            min_lon: 22.80,
            max_lat: 40.70,
            max_lon: 23.10,
        }
    }

    fn compile_fixture() -> CompileOutput {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sample_junction.osm");
        let document = crate::xml::parse_xml_file(&path).unwrap();
        let mut config = CompileConfig::new(wide_box());
        config.preset_id = Some("fixture".into());
        compile_document(&document, &config, &FlatElevation { elevation_m: 0.0 }).unwrap()
    }

    fn node_id(network: &Network, osm_id: u64) -> NodeId {
        network
            .nodes
            .iter()
            .position(|node| node.osm_id == osm_id)
            .map(|index| NodeId(index as u32))
            .unwrap_or_else(|| panic!("missing osm node {osm_id}"))
    }

    fn edge_between(network: &Network, from_osm: u64, to_osm: u64) -> (EdgeId, &Edge) {
        let from = node_id(network, from_osm);
        let to = node_id(network, to_osm);
        network
            .edges
            .iter()
            .enumerate()
            .find(|(_, edge)| edge.from == from && edge.to == to)
            .map(|(index, edge)| (EdgeId(index as u32), edge))
            .unwrap_or_else(|| panic!("missing edge {from_osm} -> {to_osm}"))
    }

    #[test]
    fn fixture_contracts_preserves_length_and_honours_lanes_speed_and_the_ban() {
        let output = compile_fixture();
        let network = &output.network;
        assert!(network.nodes.iter().all(|node| node.osm_id != 2));
        assert_eq!(network.contractions, 1);
        assert!(
            (output.length_before_contraction_m - output.length_after_contraction_m).abs() < 1e-4
        );
        assert!(output.length_after_contraction_m > 1_000.0);
        let census = network.weak_components();
        assert_eq!(census.components, 1);
        assert_eq!(census.isolated_nodes, 0);
        assert!((census.largest_length_m - network.stats.total_length_m).abs() < 1e-6);
        assert_eq!(network.stats.signals, 1);
        assert_eq!(network.stats.gateways, 0);

        let (_, trunk) = edge_between(network, 1, 3);
        assert_eq!(trunk.geometry_lonlat.len(), 3);
        assert!((trunk.geometry_lonlat[1].lon_deg - 22.940).abs() < 1e-9);
        assert!((trunk.base_free_flow_speed_mps * 3.6 - 50.0).abs() < 1e-6);
        assert_eq!(trunk.name.as_deref(), Some("Εγνατία"));
        assert_eq!(trunk.lanes, 2);
        let storage = 2.0 * (1.0 / 7.5) * trunk.length_m;
        assert!((trunk.storage_max_veh - storage).abs() < 1e-6);

        let (_, northbound) = edge_between(network, 7, 3);
        let (_, southbound) = edge_between(network, 3, 7);
        assert_eq!(northbound.lanes, 2);
        assert_eq!(southbound.lanes, 1);

        let (_, faster) = edge_between(network, 4, 5);
        assert!((faster.base_free_flow_speed_mps * 3.6 - 70.0).abs() < 1e-6);

        let roundabouts: Vec<_> = network
            .edges
            .iter()
            .filter(|edge| edge.flags.roundabout)
            .collect();
        assert!(roundabouts.len() >= 2);
        assert!(roundabouts.iter().any(|edge| {
            roundabouts.iter().any(|other| {
                edge.to == other.from && edge.from == other.to && !std::ptr::eq(*edge, *other)
            })
        }));

        let (from_id, _) = edge_between(network, 1, 3);
        let (left_id, _) = edge_between(network, 3, 6);
        let (right_id, _) = edge_between(network, 3, 7);
        let left = network
            .turns
            .iter()
            .find(|turn| turn.from_edge == from_id && turn.to_edge == left_id)
            .unwrap();
        let right = network
            .turns
            .iter()
            .find(|turn| turn.from_edge == from_id && turn.to_edge == right_id)
            .unwrap();
        assert_eq!(left.movement, Movement::Left);
        assert!(!left.allowed);
        assert_eq!(right.movement, Movement::Right);
        assert!(right.allowed);

        let (_, reversal) = edge_between(network, 12, 3);
        assert!(reversal.flags.dead_end_reversal);
        assert!(network
            .edges
            .iter()
            .all(|edge| !(edge.from == node_id(network, 3) && edge.to == node_id(network, 1))));
        for edge in &network.edges {
            if !edge.flags.dead_end_reversal {
                continue;
            }
            assert!(network.edges.iter().any(|other| {
                other.from == edge.to && other.to == edge.from && !other.flags.dead_end_reversal
            }));
        }
        assert_eq!(
            network.nodes[node_id(network, 3).index()].kind,
            NodeKind::Signalized
        );
        assert!(output.warnings.restrictions_applied >= 1);

        let temp = std::env::temp_dir().join("traffic-sim-fixture.simnet");
        let copy = std::env::temp_dir().join("traffic-sim-fixture-copy.simnet");
        store_network(&temp, network).unwrap();
        let bytes = std::fs::read(&temp).unwrap();
        let loaded = sim_core::load_network(&temp).unwrap();
        assert_eq!(loaded, *network);
        store_network(&copy, &loaded).unwrap();
        assert_eq!(bytes, std::fs::read(&copy).unwrap());
        let _ = std::fs::remove_file(&temp);
        let _ = std::fs::remove_file(&copy);
    }

    #[test]
    fn a_clipped_way_enters_through_a_synthetic_gateway() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <osm>
          <node id="1" lat="40.63" lon="22.90"/>
          <node id="2" lat="40.63" lon="22.95"/>
          <way id="10">
            <nd ref="1"/><nd ref="2"/>
            <tag k="highway" v="primary"/>
            <tag k="oneway" v="yes"/>
          </way>
        </osm>"#;
        let document = crate::xml::parse_xml_str(xml).unwrap();
        let config = CompileConfig::new(BBox {
            min_lat: 40.60,
            min_lon: 22.93,
            max_lat: 40.70,
            max_lon: 23.00,
        });
        let output =
            compile_document(&document, &config, &FlatElevation { elevation_m: 0.0 }).unwrap();
        assert_eq!(output.network.stats.gateways, 1);
        assert!(output
            .network
            .nodes
            .iter()
            .any(|node| node.synthetic && node.kind == NodeKind::Gateway));
        assert!(output.warnings.clipped_ways >= 1);
    }
}
