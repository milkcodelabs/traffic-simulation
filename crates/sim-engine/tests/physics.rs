//! Acceptance tests for spillback, mass conservation, flashing amber, and route inertia.
//! The networks are small constructed graphs with explicit geometry. The flow on them is the engine's.

use sim_core::{
    plan_signal, realize_edge, Approach, AssembleInput, BBox, DiagramFamily, Edge, EdgeDraft, EdgeFlags, EdgeId,
    EdgeScale, LonLat, Movement, Network, Node, NodeId, NodeKind, RoadClass, Scenario, SignalId, SignalSchedule, Turn,
    UtmProjection, Xy,
};
use sim_engine::{Simulation, Trip};

fn bbox() -> BBox {
    BBox {
        min_lat: 40.50,
        min_lon: 22.80,
        max_lat: 40.70,
        max_lon: 23.10,
    }
}

fn node(kind: NodeKind, x: f64, y: f64) -> Node {
    Node {
        kind,
        lonlat: LonLat {
            lon_deg: 22.95,
            lat_deg: 40.63,
        },
        xy: Xy { x_m: x, y_m: y },
        elevation_m: None,
        osm_id: 0,
        synthetic: false,
        signal: None,
    }
}

fn link(from: NodeId, to: NodeId, shape: &[(f64, f64)], class: RoadClass, lanes: u8, speed_mps: f64, way: u64) -> Edge {
    realize_edge(EdgeDraft {
        from,
        to,
        geometry_xy: shape.iter().map(|(x, y)| Xy { x_m: *x, y_m: *y }).collect(),
        geometry_lonlat: shape
            .iter()
            .map(|_| LonLat {
                lon_deg: 22.95,
                lat_deg: 40.63,
            })
            .collect(),
        elevations_m: vec![None; shape.len()],
        ignore_grade: true,
        class,
        lanes,
        posted_speed_mps: speed_mps,
        osm_way_ids: vec![way],
        name: None,
        road_ref: None,
        flags: EdgeFlags::default(),
        diagram: DiagramFamily::Triangular,
        heavy_vehicle_share: 0.08,
    })
    .expect("test link is long enough to realize")
}

fn through(node: NodeId, from: EdgeId, to: EdgeId) -> Turn {
    Turn {
        node,
        from_edge: from,
        to_edge: to,
        movement: Movement::Through,
        angle_rad: 0.0,
        allowed: true,
    }
}

fn assemble(nodes: Vec<Node>, edges: Vec<Edge>, turns: Vec<Turn>, signals: Vec<sim_core::SignalController>) -> Network {
    Network::assemble(AssembleInput {
        preset_id: None,
        bbox: bbox(),
        projection: UtmProjection::for_bbox(bbox()),
        diagram: DiagramFamily::Triangular,
        nodes,
        edges,
        turns,
        signals,
        zones: Vec::new(),
        contractions: 0,
        compiled_unix_s: 0,
    })
    .expect("test network satisfies the graph invariants")
}

#[test]
fn bottleneck_spillback_fills_upstream_links_without_losing_vehicles() {
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        node(NodeKind::Intersection, 300.0, 0.0),
        node(NodeKind::Intersection, 450.0, 0.0),
        node(NodeKind::Gateway, 600.0, 0.0),
    ];
    let edges = vec![
        link(NodeId(0), NodeId(1), &[(0.0, 0.0), (300.0, 0.0)], RoadClass::Residential, 1, 10.0, 1),
        link(NodeId(1), NodeId(2), &[(300.0, 0.0), (450.0, 0.0)], RoadClass::Residential, 1, 10.0, 2),
        link(NodeId(2), NodeId(3), &[(450.0, 0.0), (600.0, 0.0)], RoadClass::Residential, 1, 10.0, 3),
    ];
    let storage_mid = edges[1].storage_max_veh;
    assert!((storage_mid - 20.0).abs() < 1e-6, "mid-link storage {storage_mid}");
    let turns = vec![through(NodeId(1), EdgeId(0), EdgeId(1)), through(NodeId(2), EdgeId(1), EdgeId(2))];
    let network = assemble(nodes, edges, turns, Vec::new());
    let mut scenario = Scenario::baseline("closure");
    scenario.edge_capacity_scale.push(EdgeScale {
        edge: EdgeId(2),
        scale: 0.0,
    });
    let mut sim = Simulation::new(network, scenario);
    let wave: Vec<Trip> = (0..50).map(|index| Trip::new(index as f64, NodeId(0), NodeId(3))).collect();
    sim.schedule(wave);
    sim.advance(300.0, 1.0).expect("conservation while the downstream link is closed");

    assert_eq!(sim.link_vehicles(EdgeId(2)), 0, "a closed link accepts no vehicles");
    assert_eq!(sim.link_vehicles(EdgeId(1)), 20, "the feeder stops at its storage ceiling");
    assert_eq!(sim.link_vehicles(EdgeId(0)), 30, "the surplus spills into the upstream link");
    let census = sim.census();
    assert_eq!(census.spawned, 50);
    assert_eq!(census.completed, 0);
    assert_eq!(census.active, 50);
    assert_eq!(census.waiting, 0);

    sim.scenario_mut().edge_capacity_scale.clear();
    sim.advance(1_200.0, 1.0).expect("conservation after the closure lifts");
    let end = sim.census();
    assert_eq!(end.completed, 50);
    assert_eq!(end.active, 0);
    assert_eq!(end.spawned, end.active + end.completed);
    assert!(sim.link_vehicles(EdgeId(0)) == 0 && sim.link_vehicles(EdgeId(1)) == 0 && sim.link_vehicles(EdgeId(2)) == 0);
}

#[test]
fn four_thousand_vehicles_stay_conserved_until_every_trip_completes() {
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        node(NodeKind::Intersection, 2_000.0, 0.0),
        node(NodeKind::Gateway, 4_000.0, 0.0),
    ];
    let edges = vec![
        link(NodeId(0), NodeId(1), &[(0.0, 0.0), (2_000.0, 0.0)], RoadClass::Motorway, 2, 36.0, 1),
        link(NodeId(1), NodeId(2), &[(2_000.0, 0.0), (4_000.0, 0.0)], RoadClass::Motorway, 2, 36.0, 2),
    ];
    let capacity = edges[0].capacity_veh_per_s;
    let free_flow = edges[0].length_m / edges[0].free_flow_speed_mps + edges[1].length_m / edges[1].free_flow_speed_mps;
    let turns = vec![through(NodeId(1), EdgeId(0), EdgeId(1))];
    let network = assemble(nodes, edges, turns, Vec::new());
    let mut sim = Simulation::new(network, Scenario::baseline("wave"));
    let wave = vec![Trip::new(0.0, NodeId(0), NodeId(2)); 4_000];
    sim.schedule(wave);
    let mut steps = 0;
    while sim.census().completed < 4_000 && steps < 6_000 {
        sim.step(1.0).expect("conservation across the wave");
        steps += 1;
    }
    let census = sim.census();
    assert_eq!(census.spawned, 4_000);
    assert_eq!(census.completed, 4_000);
    assert_eq!(census.active, 0);
    let finished = sim.vehicles().iter().filter_map(|vehicle| vehicle.arrive_s).fold(0.0, f64::max);
    let discharge = free_flow + (4_000.0 - 1.0) / capacity;
    assert!(
        finished + 1.0 >= discharge,
        "last arrival {:.1}s is faster than the capacity bound {:.1}s",
        finished,
        discharge
    );
    assert!(
        finished < discharge + 8.0,
        "last arrival {:.1}s, capacity bound {:.1}s",
        finished,
        discharge
    );
}

#[test]
fn flashing_amber_removes_red_delay_on_the_major_approach() {
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        node(NodeKind::Signalized, 400.0, 0.0),
        node(NodeKind::Gateway, 800.0, 0.0),
        node(NodeKind::Gateway, 400.0, -200.0),
    ];
    let edges = vec![
        link(NodeId(0), NodeId(1), &[(0.0, 0.0), (400.0, 0.0)], RoadClass::Primary, 2, 50.0 / 3.6, 1),
        link(NodeId(1), NodeId(2), &[(400.0, 0.0), (800.0, 0.0)], RoadClass::Primary, 2, 50.0 / 3.6, 2),
        link(NodeId(3), NodeId(1), &[(400.0, -200.0), (400.0, 0.0)], RoadClass::Residential, 1, 30.0 / 3.6, 3),
    ];
    let east = edges[0].bearings().expect("eastbound bearing").1;
    let north = edges[2].bearings().expect("northbound bearing").1;
    let signal = plan_signal(
        NodeId(1),
        SignalId(0),
        &[
            Approach {
                edge: EdgeId(0),
                bearing_rad: east,
                lanes: edges[0].lanes,
                rank: edges[0].class.rank(),
            },
            Approach {
                edge: EdgeId(2),
                bearing_rad: north,
                lanes: edges[2].lanes,
                rank: edges[2].class.rank(),
            },
        ],
    );
    assert!(signal.phases[0].incoming.contains(&EdgeId(0)), "the major road receives the first phase");
    assert!(signal.cycle_length_s > 60.0);
    let turns = vec![
        through(NodeId(1), EdgeId(0), EdgeId(1)),
        through(NodeId(1), EdgeId(2), EdgeId(1)),
    ];
    let network = assemble(nodes, edges, turns, vec![signal]);

    let mut fixed = Scenario::baseline("fixed");
    fixed.signal_schedule = SignalSchedule::ForceFixed;
    let mut flashing = Scenario::baseline("flashing");
    flashing.signal_schedule = SignalSchedule::ForceFlashingAmber;

    let departures: Vec<Trip> = (0..36).map(|index| Trip::new(index as f64 * 5.0, NodeId(0), NodeId(2))).collect();
    let fixed_delay = run_delay(network.clone(), fixed, &departures);
    let flashing_delay = run_delay(network, flashing, &departures);
    assert!(
        flashing_delay < 3.0,
        "major-road delay under flashing amber is {flashing_delay:.2}s"
    );
    assert!(
        fixed_delay > flashing_delay + 5.0,
        "fixed-cycle delay {fixed_delay:.2}s should exceed flashing delay {flashing_delay:.2}s by the red interval"
    );
}

#[test]
fn drivers_keep_a_mild_path_and_leave_when_delay_exceeds_inertia() {
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        node(NodeKind::Intersection, 200.0, 0.0),
        node(NodeKind::Gateway, 400.0, 0.0),
    ];
    let edges = vec![
        link(NodeId(0), NodeId(1), &[(0.0, 0.0), (200.0, 0.0)], RoadClass::Residential, 1, 10.0, 1),
        link(NodeId(1), NodeId(2), &[(200.0, 0.0), (400.0, 0.0)], RoadClass::Residential, 1, 10.0, 2),
        link(
            NodeId(0),
            NodeId(2),
            &[(0.0, 0.0), (0.0, 500.0), (400.0, 0.0)],
            RoadClass::Residential,
            1,
            10.0,
            3,
        ),
    ];
    let direct = EdgeId(2);
    let choke = EdgeId(1);
    assert!(edges[2].length_m > edges[0].length_m + edges[1].length_m);
    let turns = vec![through(NodeId(1), EdgeId(0), EdgeId(1))];
    let network = assemble(nodes, edges, turns, Vec::new());
    let mut scenario = Scenario::baseline("inertia");
    scenario.edge_capacity_scale.push(EdgeScale {
        edge: choke,
        scale: 0.05,
    });

    let mut pinned = Simulation::new(network.clone(), scenario.clone());
    let mut wave: Vec<Trip> = (0..25).map(|index| Trip::new(index as f64, NodeId(0), NodeId(2)).with_inertia(1_000.0)).collect();
    wave.push(Trip::new(120.0, NodeId(0), NodeId(2)).with_inertia(1.4));
    pinned.schedule(wave);
    pinned.advance(400.0, 1.0).expect("conservation during the reroute");
    let probe = pinned.vehicles().last().expect("probe");
    assert_eq!(probe.path, vec![direct], "the probe should divert onto the long link");
    assert!(probe.arrive_s.is_some(), "the diverted trip should finish");
    assert!(
        pinned.vehicles().iter().any(|vehicle| vehicle.inertia > 10.0 && vehicle.path.contains(&choke)),
        "pinned drivers stay on the habitual path while their delay is inside the inertia band"
    );
}

fn run_delay(network: Network, scenario: Scenario, departures: &[Trip]) -> f64 {
    let mut sim = Simulation::new(network, scenario);
    sim.schedule(departures.iter().cloned());
    sim.advance(2_000.0, 1.0).expect("signal comparison conserves vehicles");
    let census = sim.census();
    assert_eq!(census.completed, departures.len() as u64);
    assert_eq!(census.active, 0);
    let mut sum = 0.0;
    for vehicle in sim.vehicles() {
        let arrive = vehicle.arrive_s.expect("completed trip has an arrival");
        sum += arrive - vehicle.depart_s - vehicle.habitual_s;
    }
    sum / departures.len() as f64
}
