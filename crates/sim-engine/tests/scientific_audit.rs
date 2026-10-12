//! Scientific audit of the mesoscopic stepper.
//!
//! Each test is an invariant, not a visual check. The networks are small and explicit
//! so a failure points at a conservation, stability, routing, or signal property.
//! The same `Simulation::step` advances a compiled city; these graphs only make the
//! boundary condition obvious.

use sim_core::{
    equilibrium_flow, plan_signal, realize_edge, Approach, AssembleInput, BBox, DiagramFamily, Edge, EdgeDraft,
    EdgeFlags, EdgeId, EdgeScale, LonLat, Movement, Network, Node, NodeId, NodeKind, RoadClass, Scenario, SignalId,
    SignalSchedule, Turn, UtmProjection, Xy,
};
use sim_engine::{boundary_demand, emit_seeded_gateway_trips, SeededCursor, Simulation, Trip};

const DT: f64 = 1.0;

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
    .expect("audit link is physically realizable")
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
    .expect("audit network satisfies the graph invariants")
}

fn straight(x0: f64, x1: f64) -> Vec<(f64, f64)> {
    vec![(x0, 0.0), (x1, 0.0)]
}

/// Directed chain. The first and last nodes are gateways. Intermediate nodes are intersections.
fn chain(lengths: &[f64], class: RoadClass, lanes: u8, speed_mps: f64) -> Network {
    let mut nodes = Vec::with_capacity(lengths.len() + 1);
    let mut edges = Vec::with_capacity(lengths.len());
    let mut x = 0.0;
    nodes.push(node(NodeKind::Gateway, 0.0, 0.0));
    for (index, length) in lengths.iter().copied().enumerate() {
        let next = x + length;
        let kind = if index + 1 == lengths.len() {
            NodeKind::Gateway
        } else {
            NodeKind::Intersection
        };
        nodes.push(node(kind, next, 0.0));
        edges.push(link(
            NodeId(index as u32),
            NodeId((index + 1) as u32),
            &straight(x, next),
            class,
            lanes,
            speed_mps,
            (index + 1) as u64,
        ));
        x = next;
    }
    let turns = (0..lengths.len().saturating_sub(1))
        .map(|index| through(NodeId((index + 1) as u32), EdgeId(index as u32), EdgeId((index + 1) as u32)))
        .collect();
    assemble(nodes, edges, turns, Vec::new())
}

fn departures(count: usize, origin: NodeId, destination: NodeId, start_s: f64, interval_s: f64) -> Vec<Trip> {
    (0..count)
        .map(|index| Trip::new(start_s + index as f64 * interval_s, origin, destination))
        .collect()
}

fn assert_step_bounds(sim: &Simulation) {
    let census = sim.census();
    assert_eq!(
        census.spawned,
        census.active + census.completed,
        "mass residual {} at t={:.0}",
        census.spawned as i64 - census.active as i64 - census.completed as i64,
        sim.time()
    );
    assert_eq!(census.active, census.on_links + census.waiting);
    assert_eq!(sim.vehicles().len() as u64, census.spawned, "a rejected trip must not occupy an id");
    assert!(sim.vht_s().is_finite() && sim.vkt_m().is_finite() && sim.delay_s().is_finite());
    assert!(sim.delay_s() >= -1e-6, "network delay {:.6}", sim.delay_s());
    let arrived = sim.vehicles().iter().filter(|vehicle| vehicle.arrive_s.is_some()).count() as u64;
    assert_eq!(arrived, census.completed);
    for (index, edge) in sim.network().edges.iter().enumerate() {
        let count = sim.link_vehicles(EdgeId(index as u32));
        assert!(
            count as f64 <= edge.storage_max_veh + 1e-6,
            "edge {index} holds {count} above storage {:.4}",
            edge.storage_max_veh
        );
        if count > 0 && edge.length_m > 0.0 {
            let density = count as f64 / edge.length_m;
            assert!(
                density <= edge.jam_density_veh_per_m() + 1e-8,
                "edge {index} density {density} exceeds jam {}",
                edge.jam_density_veh_per_m()
            );
        }
    }
    for observation in sim.observations() {
        assert!(observation.delay_s >= -1e-8);
        if observation.observed() {
            assert!(observation.mean_speed_mps.is_finite());
            assert!(observation.mean_speed_mps >= -1e-8);
        }
    }
}

fn assert_trip_delays(sim: &Simulation) {
    let mut worst = 0.0_f64;
    for vehicle in sim.vehicles() {
        let Some(arrive) = vehicle.arrive_s else { continue };
        assert!(arrive + 1e-6 >= vehicle.depart_s);
        let delay = arrive - vehicle.depart_s - vehicle.habitual_s;
        worst = worst.min(delay);
        assert!(
            delay >= -1e-4,
            "vehicle {} delay {delay:.6}s (arrive {arrive:.4}, depart {:.4}, free-flow {:.4})",
            vehicle.id,
            vehicle.depart_s,
            vehicle.habitual_s
        );
        if let Some(last) = vehicle.path.last() {
            assert_eq!(
                sim.network().edges[last.index()].to,
                vehicle.destination,
                "vehicle {} completed off its destination",
                vehicle.id
            );
        }
    }
    assert!(worst >= -1e-4);
}

fn finish(sim: &mut Simulation, expected: u64, horizon_s: f64) -> bool {
    while sim.time() + 1e-9 < horizon_s {
        sim.step(DT).expect("step");
        assert_step_bounds(sim);
        if sim.census().completed == expected && sim.census().active == 0 {
            return true;
        }
    }
    false
}

fn geh(modelled: f64, compared: f64) -> f64 {
    let sum = modelled + compared;
    if sum <= 1e-12 {
        0.0
    } else {
        (2.0 * (modelled - compared).powi(2) / sum).sqrt()
    }
}

fn theil_u(predicted: &[f64], observed: &[f64]) -> f64 {
    let n = predicted.len().min(observed.len());
    if n == 0 {
        return 0.0;
    }
    let mut square_error = 0.0;
    let mut square_predicted = 0.0;
    let mut square_observed = 0.0;
    for index in 0..n {
        let error = predicted[index] - observed[index];
        square_error += error * error;
        square_predicted += predicted[index] * predicted[index];
        square_observed += observed[index] * observed[index];
    }
    let denom = (square_predicted / n as f64).sqrt() + (square_observed / n as f64).sqrt();
    if denom <= 1e-12 {
        0.0
    } else {
        (square_error / n as f64).sqrt() / denom
    }
}

fn mean_std(values: &[f64]) -> (f64, f64) {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|value| (value - mean).powi(2)).sum::<f64>() / values.len() as f64;
    (mean, variance.sqrt())
}

fn signature(sim: &Simulation) -> (u64, u64, u64, u64, u64, Vec<u64>, Vec<Vec<u32>>) {
    let census = sim.census();
    let arrivals = sim
        .vehicles()
        .iter()
        .map(|vehicle| vehicle.arrive_s.unwrap_or(f64::NAN).to_bits())
        .collect();
    let paths = sim
        .vehicles()
        .iter()
        .map(|vehicle| vehicle.path.iter().map(|edge| edge.0).collect())
        .collect();
    (
        census.spawned,
        census.completed,
        sim.vht_s().to_bits(),
        sim.vkt_m().to_bits(),
        sim.delay_s().to_bits(),
        arrivals,
        paths,
    )
}

#[test]
fn mass_is_conserved_and_storage_bounds_a_gateway_burst() {
    let network = chain(&[300.0, 150.0, 300.0], RoadClass::Residential, 1, 10.0);
    let storage0 = network.edges[0].storage_max_veh;
    let capacity = network.edges[0].capacity_veh_per_s;
    let mut sim = Simulation::new(network, Scenario::baseline("mass"));
    let wave = 80;
    sim.schedule(departures(wave, NodeId(0), NodeId(3), 0.0, 0.0));
    sim.step(DT).expect("first step");
    assert_step_bounds(&sim);
    let burst = sim.link_vehicles(EdgeId(0));
    assert!(burst as f64 <= storage0 + 1e-6);
    assert!(burst >= 1, "the gateway must be able to load the entrance link");
    let cleared = finish(&mut sim, wave as u64, 4_000.0);
    assert!(cleared, "census {:?}", sim.census());
    assert_trip_delays(&sim);
    let census = sim.census();
    assert_eq!(census.spawned, wave as u64);
    assert_eq!(census.completed, wave as u64);
    assert_eq!(census.active, 0);
    assert_eq!(sim.rejected_trips(), 0);
    eprintln!(
        "AUDIT test=mass residual=0 spawned={spawned} burst={burst} storage0={storage0:.4} capacity={capacity:.6} burst_over_capacity={ratio:.3} max_density={density:.4} delay_s={delay:.4}",
        spawned = census.spawned,
        ratio = burst as f64 / capacity,
        density = sim.max_density(),
        delay = sim.delay_s(),
    );
}

#[test]
fn short_link_above_the_courant_limit_stays_inside_jam_and_free_flow() {
    let network = chain(&[8.0], RoadClass::Motorway, 1, 30.0);
    let edge = &network.edges[0];
    let courant = edge.free_flow_speed_mps * DT / edge.length_m;
    assert!(courant > 1.0, "Courant {courant:.3} — this case is outside a classical CFL step");
    assert!(edge.storage_max_veh <= 1.0 + 1e-6, "storage {:.4}", edge.storage_max_veh);
    let free = edge.length_m / edge.free_flow_speed_mps;
    let mut sim = Simulation::new(network, Scenario::baseline("cfl"));
    let count = 12;
    sim.schedule(departures(count, NodeId(0), NodeId(1), 0.0, 0.0));
    let mut max_on_link = 0usize;
    for _ in 0..400 {
        sim.step(DT).expect("cfl step");
        assert_step_bounds(&sim);
        max_on_link = max_on_link.max(sim.link_vehicles(EdgeId(0)));
        if sim.census().completed == count as u64 {
            break;
        }
    }
    assert_eq!(sim.census().completed, count as u64);
    assert!(max_on_link <= 1, "short-link occupancy {max_on_link}");
    let mut min_travel = f64::INFINITY;
    let mut max_speed = 0.0_f64;
    for vehicle in sim.vehicles() {
        let arrive = vehicle.arrive_s.expect("finished");
        let travel = arrive - vehicle.depart_s;
        min_travel = min_travel.min(travel);
        max_speed = max_speed.max(edge_length_over(&sim, travel));
        assert!(travel + 1e-6 >= free, "travel {travel:.4}s shorter than free-flow {free:.4}s");
    }
    assert!(max_speed <= 30.0 + 1e-6, "speed {max_speed:.4} m/s");
    assert_trip_delays(&sim);
    eprintln!(
        "AUDIT test=cfl courant={courant:.4} free_s={free:.4} min_travel_s={min_travel:.4} max_on_link={max_on_link} max_speed={max_speed:.4} storage={storage:.4}",
        storage = sim.network().edges[0].storage_max_veh,
    );
}

fn edge_length_over(sim: &Simulation, travel: f64) -> f64 {
    if travel <= 1e-9 {
        f64::INFINITY
    } else {
        sim.network().edges[0].length_m / travel
    }
}

#[test]
fn spillback_blocks_the_upstream_link_without_oscillation_or_loss() {
    let network = chain(&[300.0, 150.0, 200.0], RoadClass::Residential, 1, 10.0);
    let storage_mid = network.edges[1].storage_max_veh;
    let mid_cap = storage_mid.floor() as usize;
    let wave_time = network.edges[1].length_m / network.edges[1].wave_speed_mps;
    let mut scenario = Scenario::baseline("spillback");
    scenario.edge_capacity_scale.push(EdgeScale {
        edge: EdgeId(2),
        scale: 0.0,
    });
    let mut sim = Simulation::new(network, scenario);
    sim.schedule(departures(50, NodeId(0), NodeId(3), 0.0, 0.0));
    let mut full_at = None;
    let mut oscillations = 0u32;
    let mut previous = 0usize;
    let mut saw_full = false;
    for _ in 0..400 {
        sim.step(DT).expect("spillback step");
        assert_step_bounds(&sim);
        let mid = sim.link_vehicles(EdgeId(1));
        let closed = sim.link_vehicles(EdgeId(2));
        assert_eq!(closed, 0, "a zero-capacity link accepts no vehicle");
        assert!(mid <= mid_cap, "mid count {mid} storage cap {mid_cap}");
        if mid == mid_cap && full_at.is_none() {
            full_at = Some(sim.time());
        }
        if saw_full && mid < previous {
            oscillations += 1;
        }
        if mid == mid_cap {
            saw_full = true;
        }
        previous = mid;
    }
    let full_at = full_at.expect("the feeder reaches storage");
    assert_eq!(oscillations, 0, "mid-link count fell while the exit was closed");
    assert_eq!(sim.link_vehicles(EdgeId(1)), mid_cap);
    assert!(sim.link_vehicles(EdgeId(0)) > 0, "surplus must stand on the upstream link");
    assert_eq!(sim.census().completed, 0);
    assert_eq!(sim.census().active, 50);
    sim.scenario_mut().edge_capacity_scale.clear();
    let reopen_at = sim.time() + 2_000.0;
    let cleared = finish(&mut sim, 50, reopen_at);
    assert!(cleared, "census {:?}", sim.census());
    assert_trip_delays(&sim);
    eprintln!(
        "AUDIT test=spillback full_at_s={full_at:.0} oscillations={oscillations} wave_time_s={wave_time:.2} upstream_at_full={up} mid={mid_cap} recovered=1 max_density={density:.4}",
        up = 50 - mid_cap,
        density = sim.max_density(),
    );
}

#[test]
fn one_link_queue_is_fifo_and_discharge_tracks_capacity() {
    let network = chain(&[800.0], RoadClass::Residential, 1, 10.0);
    let edge = &network.edges[0];
    let free = edge.length_m / edge.free_flow_speed_mps;
    let capacity = edge.capacity_veh_per_s;
    let count = 60usize;
    let mut sim = Simulation::new(network, Scenario::baseline("fifo"));
    sim.schedule(departures(count, NodeId(0), NodeId(1), 0.0, 0.5));
    let mut completed_at_100 = None;
    let mut completed_at_200 = None;
    let mut previous_arrive = 0.0;
    let mut order_breaks = 0u32;
    while sim.time() + 1e-9 < 500.0 && sim.census().completed < count as u64 {
        sim.step(DT).expect("fifo step");
        assert_step_bounds(&sim);
        if (sim.time() - 100.0).abs() < 0.5 && completed_at_100.is_none() {
            completed_at_100 = Some(sim.census().completed);
        }
        if (sim.time() - 200.0).abs() < 0.5 && completed_at_200.is_none() {
            completed_at_200 = Some(sim.census().completed);
        }
    }
    assert_eq!(sim.census().completed, count as u64, "not all vehicles finished by t={:.0}", sim.time());
    for vehicle in sim.vehicles() {
        let arrive = vehicle.arrive_s.expect("arrival");
        if vehicle.id > 0 && arrive + 1e-6 < previous_arrive {
            order_breaks += 1;
        }
        previous_arrive = arrive;
        assert!(arrive + 1e-6 >= vehicle.depart_s + free);
    }
    assert_eq!(order_breaks, 0);
    let early = completed_at_100.expect("sample at 100s") as f64;
    let late = completed_at_200.expect("sample at 200s") as f64;
    let discharged = late - early;
    let expected = capacity * 100.0;
    assert!(
        (discharged - expected).abs() <= 3.0,
        "discharged {discharged:.2} vs capacity integral {expected:.2}"
    );
    assert_trip_delays(&sim);
    eprintln!(
        "AUDIT test=fifo order_breaks=0 discharged_100s={discharged:.2} capacity_integral={expected:.2} free_s={free:.2} min_delay_ok=1"
    );
}

#[test]
fn diagram_peaks_at_capacity_and_the_queue_sending_flow_departs_from_the_congested_branch() {
    let free = 15.0;
    let wave = 5.0;
    let jam = 0.15;
    let capacity = free * jam * wave / (free + wave);
    let mut fd_rmse = 0.0;
    let mut samples = 0.0;
    for step in 0..=20 {
        let density = jam * step as f64 / 20.0;
        let flow = equilibrium_flow(DiagramFamily::Triangular, free, capacity, jam, density);
        assert!(flow.is_finite() && flow >= -1e-12);
        assert!(flow <= capacity + 1e-8);
        let queue_sending = if density <= 0.0 { 0.0 } else { capacity.min(free * density) };
        if density > capacity / free {
            fd_rmse += (flow - queue_sending).powi(2);
            samples += 1.0;
        }
    }
    fd_rmse = (fd_rmse / samples).sqrt();
    let critical = capacity / free;
    let peak = equilibrium_flow(DiagramFamily::Triangular, free, capacity, jam, critical);
    assert!((peak - capacity).abs() <= 1e-8 * capacity);
    assert_eq!(equilibrium_flow(DiagramFamily::Triangular, free, capacity, jam, 0.0), 0.0);
    assert_eq!(equilibrium_flow(DiagramFamily::Triangular, free, capacity, jam, jam), 0.0);
    eprintln!(
        "AUDIT test=diagram peak={peak:.6} capacity={capacity:.6} congested_rmse_vs_spatial_queue={fd_rmse:.6} samples={samples:.0}"
    );
}

#[test]
fn bounded_rationality_uses_both_corridors_without_path_flip_flop() {
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        node(NodeKind::Intersection, 600.0, 0.0),
        node(NodeKind::Gateway, 1_000.0, 0.0),
    ];
    let edges = vec![
        link(NodeId(0), NodeId(1), &straight(0.0, 600.0), RoadClass::Residential, 1, 10.0, 1),
        link(NodeId(1), NodeId(2), &straight(600.0, 1_000.0), RoadClass::Residential, 1, 10.0, 2),
        link(
            NodeId(1),
            NodeId(2),
            &[(600.0, 0.0), (600.0, 500.0), (1_000.0, 0.0)],
            RoadClass::Residential,
            1,
            10.0,
            3,
        ),
    ];
    let short = EdgeId(1);
    let long = EdgeId(2);
    assert!(edges[2].length_m > edges[1].length_m + 300.0);
    let turns = vec![through(NodeId(1), EdgeId(0), short), through(NodeId(1), EdgeId(0), long)];
    let network = assemble(nodes, edges, turns, Vec::new());
    let capacity = network.edges[1].capacity_veh_per_s;
    let mut scenario = Scenario::baseline("dta");
    scenario.edge_capacity_scale.push(EdgeScale {
        edge: EdgeId(1),
        scale: 0.25,
    });
    let mut sim = Simulation::new(network, scenario);
    let mut trips = departures(420, NodeId(0), NodeId(2), 0.0, 1.0 / (1.6 * capacity));
    trips[3] = Trip::new(trips[3].depart_s, NodeId(0), NodeId(2)).with_inertia(1_000.0);
    let pinned_depart = trips[3].depart_s;
    let expected = trips.len() as u64;
    sim.schedule(trips);
    let mut shares = Vec::new();
    let mut last_majority_long = None;
    let mut flips = 0u32;
    while sim.time() + 1e-9 < 2_200.0 && sim.census().completed < expected {
        sim.step(DT).expect("dta step");
        assert_step_bounds(&sim);
        if sim.time() % 30.0 < 0.5 {
            let on_short = sim.link_vehicles(short) as f64;
            let on_long = sim.link_vehicles(long) as f64;
            let share = if on_short + on_long > 0.0 {
                on_long / (on_short + on_long)
            } else {
                shares.last().copied().unwrap_or(0.0)
            };
            if let Some(previous) = last_majority_long {
                let majority = share >= 0.5;
                if majority != previous && on_short + on_long > 0.0 {
                    flips += 1;
                }
            }
            if on_short + on_long > 0.0 {
                last_majority_long = Some(share >= 0.5);
            }
            shares.push(share);
        }
    }
    assert_eq!(sim.census().active, 0, "DTA run did not clear, census {:?}", sim.census());
    assert_trip_delays(&sim);
    let mut used_short = 0u32;
    let mut used_long = 0u32;
    let mut pinned_on_short = false;
    for vehicle in sim.vehicles() {
        let on_short = vehicle.path.contains(&short);
        let on_long = vehicle.path.contains(&long);
        if on_short {
            used_short += 1;
        }
        if on_long {
            used_long += 1;
        }
        if (vehicle.depart_s - pinned_depart).abs() < 1e-9 && vehicle.inertia > 10.0 {
            pinned_on_short = on_short;
            assert!(!on_long, "inertia 1000 must not divert");
        }
    }
    assert!(pinned_on_short, "pinned driver left the habitual corridor");
    assert!(used_short > 0 && used_long > 0, "short {used_short} long {used_long}");
    let persistence_pred: Vec<f64> = shares.iter().copied().take(shares.len().saturating_sub(1)).collect();
    let persistence_obs: Vec<f64> = shares.iter().copied().skip(1).collect();
    let theil = theil_u(&persistence_pred, &persistence_obs);
    assert!(flips <= 12, "route-share flips {flips}");
    assert!(theil.is_finite());
    eprintln!(
        "AUDIT test=dta used_short={used_short} used_long={used_long} flips={flips} theil_u={theil:.4} samples={} pinned_stayed=1",
        shares.len()
    );
}

#[test]
fn signal_delay_stays_non_negative_across_the_flashing_boundary() {
    let network = signal_approaches();
    let capacity = network.edges[0].capacity_veh_per_s;
    let storage_cap = network
        .edges
        .iter()
        .map(|edge| edge.storage_max_veh.floor() as i64)
        .max()
        .unwrap_or(1);
    let mut sim = Simulation::new(network, Scenario::baseline("signals"));
    sim.set_clock_origin(6.0 * 3600.0 - 40.0);
    let mut trips = departures(16, NodeId(0), NodeId(2), 0.0, 3.0);
    trips.extend(departures(8, NodeId(3), NodeId(2), 1.0, 6.0));
    let expected = trips.len() as u64;
    sim.schedule(trips);
    let mut previous = [0usize; 3];
    let mut max_jump = 0i64;
    let mut mode_jump = 0i64;
    while sim.time() + 1e-9 < 2_000.0 && sim.census().completed < expected {
        let before = sim.time();
        sim.step(DT).expect("signal step");
        assert_step_bounds(&sim);
        for index in 0..3 {
            let count = sim.link_vehicles(EdgeId(index as u32)) as i64;
            let jump = (count - previous[index] as i64).abs();
            max_jump = max_jump.max(jump);
            if before < 40.0 && sim.time() >= 40.0 {
                mode_jump = mode_jump.max(jump);
            }
            previous[index] = count as usize;
        }
    }
    assert_eq!(sim.census().completed, expected, "signal trips did not finish");
    assert_eq!(sim.census().active, 0);
    assert_trip_delays(&sim);
    assert!(max_jump <= storage_cap, "occupancy jump {max_jump} above storage {storage_cap}");
    let slot_ceiling = capacity.ceil() as i64 + 4;
    assert!(
        mode_jump <= slot_ceiling,
        "mode-boundary occupancy jump {mode_jump} exceeds one discharge step ({slot_ceiling})"
    );
    let mut worst = f64::MAX;
    for vehicle in sim.vehicles() {
        let arrive = vehicle.arrive_s.expect("done");
        worst = worst.min(arrive - vehicle.depart_s - vehicle.habitual_s);
    }
    eprintln!(
        "AUDIT test=signals max_occupancy_jump={max_jump} boundary_jump={mode_jump} min_trip_delay_s={worst:.4} capacity={capacity:.4} negatives=0"
    );
}

#[test]
fn identical_seeds_are_bit_exact_including_the_parallel_node_path() {
    let sequential = signal_pair_signature();
    let again = signal_pair_signature();
    assert_eq!(sequential, again);
    let parallel = parallel_signature();
    let parallel_again = parallel_signature();
    assert_eq!(parallel.0, parallel_again.0, "parallel census/totals diverged");
    assert_eq!(parallel.1, parallel_again.1, "parallel arrivals diverged");
    let mut max_geh = 0.0_f64;
    for (left, right) in parallel.2.iter().zip(&parallel_again.2) {
        max_geh = max_geh.max(geh(*left as f64, *right as f64));
    }
    assert_eq!(max_geh, 0.0);
    eprintln!(
        "AUDIT test=determinism signal_runs_equal=1 parallel_links=70 max_geh={max_geh:.1} vht_bits_equal=1"
    );
}

fn signal_pair_signature() -> (u64, u64, u64, u64, u64, Vec<u64>, Vec<Vec<u32>>) {
    let network = chain(&[400.0, 400.0], RoadClass::Primary, 2, 50.0 / 3.6);
    let mut sim = Simulation::new(network, Scenario::baseline("det"));
    sim.schedule(departures(25, NodeId(0), NodeId(2), 0.0, 2.0));
    assert!(finish(&mut sim, 25, 1_500.0), "determinism corridor did not clear");
    assert_eq!(sim.census().active, 0);
    signature(&sim)
}

fn parallel_signature() -> ((u64, u64, u64), Vec<u64>, Vec<u32>) {
    let count = 70usize;
    let mut nodes = Vec::with_capacity(count * 2);
    let mut edges = Vec::with_capacity(count);
    for index in 0..count {
        let x = index as f64 * 20.0;
        nodes.push(node(NodeKind::Gateway, x, 0.0));
        nodes.push(node(NodeKind::Gateway, x + 10.0, 0.0));
        edges.push(link(
            NodeId((index * 2) as u32),
            NodeId((index * 2 + 1) as u32),
            &[(x, 0.0), (x + 80.0, 0.0)],
            RoadClass::Residential,
            1,
            10.0,
            (index + 1) as u64,
        ));
    }
    let network = assemble(nodes, edges, Vec::new(), Vec::new());
    let mut sim = Simulation::new(network, Scenario::baseline("parallel"));
    let mut trips = Vec::new();
    for index in 0..count {
        let origin = NodeId((index * 2) as u32);
        let destination = NodeId((index * 2 + 1) as u32);
        trips.extend(departures(3, origin, destination, 0.0, 1.0));
    }
    sim.schedule(trips);
    for _ in 0..120 {
        sim.step(DT).expect("parallel step");
        assert_step_bounds(&sim);
    }
    let census = sim.census();
    let arrivals = sim
        .vehicles()
        .iter()
        .map(|vehicle| vehicle.arrive_s.unwrap_or(-1.0).to_bits())
        .collect();
    let loads: Vec<u32> = (0..count).map(|index| sim.link_vehicles(EdgeId(index as u32)) as u32).collect();
    ((census.spawned, census.completed, sim.vht_s().to_bits()), arrivals, loads)
}

#[test]
fn nearby_seeds_move_network_metrics_continuously() {
    let network = chain(&[500.0, 500.0], RoadClass::Residential, 1, 10.0);
    let capacity = network.edges[0].capacity_veh_per_s;
    let uncongested: Vec<f64> = (1..=5)
        .map(|seed| seeded_metric(&network, seed, capacity * 0.25 * 3600.0, 300.0))
        .collect();
    let congested: Vec<f64> = (1..=5)
        .map(|seed| seeded_metric(&network, seed, capacity * 1.8 * 3600.0, 240.0))
        .collect();
    let (mean_u, std_u) = mean_std(&uncongested);
    let (mean_c, std_c) = mean_std(&congested);
    let cv_u = std_u / mean_u.max(1e-9);
    let cv_c = std_c / mean_c.max(1e-9);
    assert!(uncongested.iter().all(|value| value.is_finite()));
    assert!(congested.iter().all(|value| value.is_finite()));
    assert!(cv_u < 0.08, "uncongested cv {cv_u:.4} values {uncongested:?}");
    assert!(cv_c < 0.75, "congested cv {cv_c:.4} values {congested:?}");
    let spread = congested.iter().cloned().fold(0.0_f64, f64::max) / congested.iter().cloned().fold(f64::MAX, f64::min);
    assert!(spread < 2.5, "congested max/min {spread:.3}");
    let identical = seeded_metric(&network, 7, capacity * 3600.0, 180.0);
    let identical_again = seeded_metric(&network, 7, capacity * 3600.0, 180.0);
    assert!((identical - identical_again).abs() <= 1e-9);
    eprintln!(
        "AUDIT test=seeds uncongested_cv={cv_u:.6} congested_cv={cv_c:.6} congested_max_over_min={spread:.4} same_seed_delta=0 mean_u={mean_u:.4} mean_c={mean_c:.4}"
    );
}

fn seeded_metric(network: &Network, seed: u64, vehicles_per_hour: f64, horizon_s: f64) -> f64 {
    let mut sim = Simulation::new(network.clone(), Scenario::baseline("seed"));
    let mut cursor = SeededCursor::default();
    let trips = emit_seeded_gateway_trips(
        &[NodeId(0)],
        &[NodeId(2)],
        vehicles_per_hour,
        seed,
        &[1.0],
        &mut cursor,
        horizon_s,
    );
    let spawned = trips.len() as f64;
    sim.schedule(trips);
    sim.advance(horizon_s, DT).expect("seeded run");
    assert_step_bounds(&sim);
    if spawned <= 0.0 {
        0.0
    } else {
        sim.vht_s() / spawned
    }
}

#[test]
fn saturation_at_two_and_a_half_times_capacity_clears_inside_two_hours() {
    let network = chain(&[400.0, 250.0, 400.0], RoadClass::Residential, 1, 10.0);
    let capacity = network.edges[1].capacity_veh_per_s;
    let load_s = 600.0;
    let recovery_s = 7_200.0;
    let rate = 2.5 * capacity;
    let mut sim = Simulation::new(network, Scenario::baseline("gridlock"));
    sim.schedule(departures(
        (rate * load_s).ceil() as usize,
        NodeId(0),
        NodeId(3),
        0.0,
        1.0 / rate,
    ));
    let mut clear_s = None;
    while sim.time() + 1e-9 < load_s + recovery_s {
        sim.step(DT).expect("stress step");
        assert_step_bounds(&sim);
        if sim.time() >= load_s && sim.census().active == 0 && sim.census().spawned > 0 {
            clear_s = Some(sim.time());
            break;
        }
    }
    let clear_s = clear_s.expect("network did not clear");
    assert!(sim.max_density() >= 0.9, "peak density {:.3} never jammed a link", sim.max_density());
    assert_eq!(sim.census().completed, sim.census().spawned);
    assert_eq!(sim.rejected_trips(), 0);
    assert_trip_delays(&sim);
    eprintln!(
        "AUDIT test=stress load_s={load_s:.0} demand_over_capacity=2.5 clear_s={clear_s:.0} recovery_used_s={used:.0} max_density={density:.4} spawned={spawned}",
        used = clear_s - load_s,
        density = sim.max_density(),
        spawned = sim.census().spawned,
    );
}

#[test]
fn crossing_approaches_clear_after_demand_stops() {
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        node(NodeKind::Intersection, 400.0, 0.0),
        node(NodeKind::Gateway, 800.0, 0.0),
        node(NodeKind::Gateway, 400.0, -400.0),
        node(NodeKind::Gateway, 400.0, 400.0),
    ];
    let edges = vec![
        link(NodeId(0), NodeId(1), &[(0.0, 0.0), (400.0, 0.0)], RoadClass::Residential, 1, 10.0, 1),
        link(NodeId(1), NodeId(2), &[(400.0, 0.0), (800.0, 0.0)], RoadClass::Residential, 1, 10.0, 2),
        link(NodeId(3), NodeId(1), &[(400.0, -400.0), (400.0, 0.0)], RoadClass::Residential, 1, 10.0, 3),
        link(NodeId(1), NodeId(4), &[(400.0, 0.0), (400.0, 400.0)], RoadClass::Residential, 1, 10.0, 4),
    ];
    let capacity = edges[1].capacity_veh_per_s;
    let turns = vec![
        through(NodeId(1), EdgeId(0), EdgeId(1)),
        through(NodeId(1), EdgeId(0), EdgeId(3)),
        through(NodeId(1), EdgeId(2), EdgeId(1)),
        through(NodeId(1), EdgeId(2), EdgeId(3)),
    ];
    let network = assemble(nodes, edges, turns, Vec::new());
    let mut sim = Simulation::new(network, Scenario::baseline("cross"));
    let load_s = 300.0;
    let rate = 2.5 * capacity;
    let per_approach = (rate * load_s).ceil() as usize;
    let mut trips = departures(per_approach, NodeId(0), NodeId(2), 0.0, 1.0 / rate);
    trips.extend(departures(per_approach, NodeId(3), NodeId(4), 0.2, 1.0 / rate));
    sim.schedule(trips);
    let mut clear_s = None;
    while sim.time() + 1e-9 < load_s + 7_200.0 {
        sim.step(DT).expect("cross step");
        assert_step_bounds(&sim);
        if sim.time() >= load_s && sim.census().active == 0 {
            clear_s = Some(sim.time());
            break;
        }
    }
    let clear_s = clear_s.expect("crossing did not clear");
    assert_trip_delays(&sim);
    eprintln!(
        "AUDIT test=crossing clear_s={clear_s:.0} spawned={} max_density={:.4}",
        sim.census().spawned,
        sim.max_density()
    );
}

#[test]
fn severed_corridor_queues_upstream_and_diverts_when_an_exit_exists() {
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        node(NodeKind::Intersection, 250.0, 0.0),
        node(NodeKind::Gateway, 370.0, 0.0),
    ];
    let edges = vec![
        link(NodeId(0), NodeId(1), &straight(0.0, 250.0), RoadClass::Residential, 1, 10.0, 1),
        link(NodeId(1), NodeId(2), &straight(250.0, 370.0), RoadClass::Residential, 1, 10.0, 2),
        link(
            NodeId(1),
            NodeId(2),
            &[(250.0, 0.0), (250.0, 500.0), (370.0, 0.0)],
            RoadClass::Residential,
            1,
            10.0,
            3,
        ),
    ];
    assert!(edges[1].length_m < 300.0, "the closure must sit inside the usual inertia gate");
    let turns = vec![
        through(NodeId(1), EdgeId(0), EdgeId(1)),
        through(NodeId(1), EdgeId(0), EdgeId(2)),
    ];
    let network = assemble(nodes, edges, turns, Vec::new());

    let mut open = Simulation::new(network.clone(), Scenario::baseline("pre-closure"));
    open.schedule(departures(8, NodeId(0), NodeId(2), 0.0, 1.0));
    open.advance(1.0, DT).expect("load the approach");
    open.scenario_mut().edge_capacity_scale.push(EdgeScale {
        edge: EdgeId(1),
        scale: 0.0,
    });
    let cleared = finish(&mut open, 8, 800.0);
    assert!(
        cleared,
        "census {:?} routes {} reroutes {} paths {:?} counts [{}, {}, {}]",
        open.census(),
        open.route_searches(),
        open.reroute_searches(),
        open.vehicles()
            .iter()
            .map(|vehicle| vehicle.path.iter().map(|edge| edge.0).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        open.link_vehicles(EdgeId(0)),
        open.link_vehicles(EdgeId(1)),
        open.link_vehicles(EdgeId(2)),
    );
    assert_eq!(open.link_vehicles(EdgeId(1)), 0);
    assert!(open.vehicles().iter().all(|vehicle| !vehicle.path.contains(&EdgeId(1))));
    assert!(open.vehicles().iter().any(|vehicle| vehicle.path.contains(&EdgeId(2))));
    assert_trip_delays(&open);

    let mut forced = Scenario::baseline("already-closed");
    forced.edge_capacity_scale.push(EdgeScale {
        edge: EdgeId(1),
        scale: 0.0,
    });
    let mut held = Simulation::new(network.clone(), forced);
    held.schedule(departures(6, NodeId(0), NodeId(2), 0.0, 0.0));
    held.advance(30.0, DT).expect("closure from the start");
    assert_step_bounds(&held);
    assert_eq!(held.link_vehicles(EdgeId(1)), 0);
    assert!(held.vehicles().iter().all(|vehicle| vehicle.path.first() != Some(&EdgeId(1))));

    let only = chain(&[200.0], RoadClass::Residential, 1, 10.0);
    let mut trapped = Scenario::baseline("only-path");
    trapped.edge_capacity_scale.push(EdgeScale {
        edge: EdgeId(0),
        scale: -1.0,
    });
    let mut queued = Simulation::new(only, trapped);
    queued.schedule(departures(5, NodeId(0), NodeId(1), 0.0, 0.0));
    queued.advance(20.0, DT).expect("zero and negative scale must not panic");
    assert_step_bounds(&queued);
    assert_eq!(queued.census().completed, 0);
    assert_eq!(queued.census().active, 5);
    assert_eq!(queued.link_vehicles(EdgeId(0)), 0, "negative scale is a closure, not a crash");
    queued.scenario_mut().edge_capacity_scale.clear();
    assert!(finish(&mut queued, 5, 400.0));
    eprintln!(
        "AUDIT test=severance diverted=8 stayed_off_closed=1 negative_scale_panic=0 only_path_queued=5 recovered_after_reopen=1"
    );
}

#[test]
fn fixed_cycle_and_flashing_amber_do_not_invent_negative_delay() {
    let network = signal_approaches();
    let mut fixed = Scenario::baseline("fixed");
    fixed.signal_schedule = SignalSchedule::ForceFixed;
    let mut flashing = Scenario::baseline("flash");
    flashing.signal_schedule = SignalSchedule::ForceFlashingAmber;
    let trips = departures(20, NodeId(0), NodeId(2), 0.0, 4.0);
    let fixed_delay = run_mean_delay(network.clone(), fixed, &trips);
    let flashing_delay = run_mean_delay(network, flashing, &trips);
    assert!(fixed_delay >= -1e-6 && flashing_delay >= -1e-6);
    assert!(flashing_delay < fixed_delay, "flashing {flashing_delay:.3} fixed {fixed_delay:.3}");
    eprintln!(
        "AUDIT test=signal_modes fixed_delay_s={fixed_delay:.4} flashing_delay_s={flashing_delay:.4} gap_s={gap:.4}",
        gap = fixed_delay - flashing_delay
    );
}

fn signal_approaches() -> Network {
    let mut center = node(NodeKind::Signalized, 400.0, 0.0);
    center.signal = Some(SignalId(0));
    let edges = vec![
        link(NodeId(0), NodeId(1), &[(0.0, 0.0), (400.0, 0.0)], RoadClass::Primary, 2, 50.0 / 3.6, 1),
        link(NodeId(1), NodeId(2), &[(400.0, 0.0), (800.0, 0.0)], RoadClass::Primary, 2, 50.0 / 3.6, 2),
        link(NodeId(3), NodeId(1), &[(400.0, -200.0), (400.0, 0.0)], RoadClass::Residential, 1, 30.0 / 3.6, 3),
    ];
    let east = edges[0].bearings().expect("bearing").1;
    let north = edges[2].bearings().expect("bearing").1;
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
    let nodes = vec![
        node(NodeKind::Gateway, 0.0, 0.0),
        center,
        node(NodeKind::Gateway, 800.0, 0.0),
        node(NodeKind::Gateway, 400.0, -200.0),
    ];
    let turns = vec![
        through(NodeId(1), EdgeId(0), EdgeId(1)),
        through(NodeId(1), EdgeId(2), EdgeId(1)),
    ];
    assemble(nodes, edges, turns, vec![signal])
}

fn run_mean_delay(network: Network, scenario: Scenario, trips: &[Trip]) -> f64 {
    let mut sim = Simulation::new(network, scenario);
    sim.schedule(trips.iter().cloned());
    assert!(finish(&mut sim, trips.len() as u64, 2_500.0), "mode run census {:?}", sim.census());
    assert_trip_delays(&sim);
    let mut sum = 0.0;
    for vehicle in sim.vehicles() {
        let arrive = vehicle.arrive_s.expect("done");
        sum += arrive - vehicle.depart_s - vehicle.habitual_s;
    }
    sum / trips.len() as f64
}

fn load_thessaloniki() -> Network {
    let path = [
        "data/cache/thessaloniki.simnet",
        "../data/cache/thessaloniki.simnet",
        "../../data/cache/thessaloniki.simnet",
    ]
    .into_iter()
    .map(std::path::Path::new)
    .find(|path| path.exists())
    .expect("compiled Thessaloniki cache");
    sim_core::load_network(path).expect("Thessaloniki cache decodes")
}

#[test]
fn thessaloniki_sample_conserves_mass_and_repeats() {
    let network = load_thessaloniki();
    let mut max_courant = 0.0_f64;
    let mut over_cfl = 0usize;
    let mut min_length = f64::INFINITY;
    for edge in &network.edges {
        min_length = min_length.min(edge.length_m);
        let courant = edge.free_flow_speed_mps * DT / edge.length_m.max(1e-9);
        max_courant = max_courant.max(courant);
        if courant > 1.0 {
            over_cfl += 1;
        }
    }
    let scenario = Scenario::baseline("thessaloniki-audit");
    let boundary = boundary_demand(&network, &scenario);
    let mut trips = Vec::new();
    for (index, origin) in boundary.origins.iter().enumerate().take(48) {
        let Some(destination) = boundary
            .destinations
            .iter()
            .rev()
            .find(|destination| boundary.allows(*origin, **destination))
            .copied()
        else {
            continue;
        };
        trips.push(Trip::new(index as f64 * 3.0, *origin, destination));
    }
    assert!(trips.len() >= 16, "routable boundary trips {}", trips.len());
    let first = run_city(&network, &scenario, &trips, 240);
    let second = run_city(&network, &scenario, &trips, 240);
    assert_eq!(first.0, second.0, "Thessaloniki totals diverged");
    assert_eq!(first.1, second.1, "Thessaloniki arrivals diverged");
    let mut max_geh = 0.0_f64;
    for (left, right) in first.2.iter().zip(&second.2) {
        max_geh = max_geh.max(geh(*left, *right));
    }
    assert_eq!(max_geh, 0.0);

    let mut closed = scenario.clone();
    let severed = network
        .edges
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.length_m.total_cmp(&right.1.length_m))
        .map(|(index, _)| EdgeId(index as u32))
        .expect("edge");
    closed.edge_capacity_scale.push(EdgeScale {
        edge: severed,
        scale: 0.0,
    });
    let mut sim = Simulation::new(network.clone(), closed);
    sim.schedule(trips.iter().take(12).cloned());
    for _ in 0..60 {
        sim.step(DT).expect("a zero-capacity city link must not panic");
        assert_step_bounds(&sim);
    }
    let census = sim.census();
    assert_eq!(census.spawned, census.active + census.completed);
    eprintln!(
        "AUDIT test=thessaloniki nodes={} edges={} signals={} min_length_m={min_length:.2} max_courant={max_courant:.3} links_over_cfl={over_cfl} sample_spawned={} sample_completed={} sample_geh={max_geh:.1} closure_edge={} closure_residual=0",
        network.stats.nodes,
        network.stats.edges,
        network.stats.signals,
        first.3,
        first.4,
        severed.0,
    );
}

fn run_city(network: &Network, scenario: &Scenario, trips: &[Trip], steps: u32) -> ((u64, u64, u64), Vec<u64>, Vec<f64>, u64, u64) {
    let mut sim = Simulation::new(network.clone(), scenario.clone());
    sim.schedule(trips.iter().cloned());
    for _ in 0..steps {
        sim.step(DT).expect("city step");
        assert_step_bounds(&sim);
    }
    let census = sim.census();
    assert_eq!(census.spawned, census.active + census.completed);
    let arrivals = sim
        .vehicles()
        .iter()
        .map(|vehicle| vehicle.arrive_s.unwrap_or(f64::NAN).to_bits())
        .collect();
    let loads: Vec<f64> = (0..network.edges.len().min(256))
        .map(|index| sim.link_vehicles(EdgeId(index as u32)) as f64)
        .collect();
    (
        (census.spawned, census.completed, sim.vht_s().to_bits()),
        arrivals,
        loads,
        census.spawned,
        census.completed,
    )
}
