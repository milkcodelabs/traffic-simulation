//! Headless performance harness for a compiled network.
//!
//! Tick windows use Δt = 1 s. Vehicles are released together, the clock is walked
//! forward so the first links can discharge, and only the following window is timed.
//! Thread counts are separate processes so the Rayon pool is built at that width.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde_json::{json, Value};
use sim_core::{
    load_network, DiurnalProfile, Edge, EdgeId, LonLat, Network, Node, NodeId, Phase, Scenario, SignalController,
    SignalSchedule, Turn, Xy, Zone,
};
use sim_engine::{
    boundary_demand, emit_seeded_gateway_trips, gateway_zone_scales, local_router_scratch_bytes, retain_routable,
    Simulation, Trip, SeededCursor,
};
use sim_ipc::{decode_frame, encode_frame, top_bottlenecks, FrameHeader};

use crate::alloc_count;

const LOW_VEHICLES: u64 = 3_500;
const PEAK_VEHICLES: u64 = 30_000;
const GRIDLOCK_VEHICLES: u64 = 80_000;
const PUBLISH_ITERS: u32 = 20;

pub struct BenchRequest {
    pub cache: PathBuf,
    pub worker: Option<String>,
    pub threads: Option<usize>,
    pub vehicles: Option<u64>,
    pub measure_ticks: u32,
    pub warmup_s: f64,
    pub daily_vehicles: f64,
    pub horizon_s: f64,
    pub seed: u64,
    pub output: PathBuf,
    pub only: String,
}

pub fn dispatch(request: BenchRequest) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(worker) = request.worker.as_deref() {
        match worker {
            "tick" => worker_tick(&request),
            "day" => worker_day(&request),
            other => Err(format!("unknown benchmark worker {other}").into()),
        }
    } else {
        run_suite(request)
    }
}

fn run_suite(request: BenchRequest) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = request.output.parent() {
        fs::create_dir_all(parent)?;
    }
    let native = native_threads();
    let mut doc = json!({
        "methodology": methodology(),
        "host": host_info(),
        "build": {
            "optimized": !cfg!(debug_assertions),
            "debug_assertions": cfg!(debug_assertions),
        },
        "protocol": {
            "dt_s": 1.0,
            "measure_ticks": request.measure_ticks,
            "warmup_s": request.warmup_s,
            "loads": [LOW_VEHICLES, PEAK_VEHICLES, GRIDLOCK_VEHICLES],
            "daily_vehicles": request.daily_vehicles,
            "horizon_s": request.horizon_s,
            "seed": request.seed,
            "signal_load": "force fixed cycle, clock origin 08:00",
            "signal_day": "compiled plans, flashing amber 00:00-06:00",
        },
        "runs": [],
    });
    if cfg!(debug_assertions) {
        eprintln!("warning: this binary is unoptimized; do not publish these timings");
    }
    eprintln!("benchmark host {}", doc["host"]["cpu"].as_str().unwrap_or("unknown"));
    if request.only != "day" {
        eprintln!("benchmark static {}", request.cache.display());
        let static_part = measure_static(&request.cache)?;
        doc["static"] = static_part;
        write_doc(&request.output, &doc)?;
    }
    if let Some(vehicles) = request.vehicles {
        if request.only != "day" {
            let threads = request.threads.unwrap_or(native);
            let run = spawn_tick(&request, vehicles, threads)?;
            doc["runs"].as_array_mut().expect("runs").push(run);
            write_doc(&request.output, &doc)?;
        }
        return Ok(());
    }
    if matches!(request.only.as_str(), "all" | "loads" | "scale") {
        let loads = load_plan(&request.only);
        let threads = thread_plan(&request.only, request.threads, native);
        for vehicles in loads {
            for thread_count in &threads {
                let run = spawn_tick(&request, vehicles, *thread_count)?;
                doc["runs"].as_array_mut().expect("runs").push(run);
                write_doc(&request.output, &doc)?;
            }
        }
    }
    if matches!(request.only.as_str(), "all" | "day") {
        let threads = request.threads.unwrap_or(native);
        eprintln!(
            "benchmark day  daily {:.0}  horizon {:.0}s  threads {threads}",
            request.daily_vehicles, request.horizon_s
        );
        let day = spawn_day(&request, threads)?;
        doc["day"] = day;
        write_doc(&request.output, &doc)?;
    }
    eprintln!("benchmark wrote {}", request.output.display());
    Ok(())
}

fn methodology() -> &'static str {
    "Loads release the target number of routable gateway trips at t = 0 on the compiled network, walk warmup_s of simulation time so the first links can discharge, then time measure_ticks calls of step(1.0). Stage times come from the stepper's own timers over that window. Allocation counts are armed only around those steps. Each thread count is a fresh process whose Rayon pool is built at that width. The 24-hour run is the diurnal boundary instrument, the same loop the headless experiment uses."
}

fn load_plan(only: &str) -> Vec<u64> {
    if only == "scale" {
        vec![LOW_VEHICLES, PEAK_VEHICLES, GRIDLOCK_VEHICLES]
    } else {
        vec![LOW_VEHICLES, PEAK_VEHICLES, GRIDLOCK_VEHICLES]
    }
}

fn thread_plan(only: &str, requested: Option<usize>, native: usize) -> Vec<usize> {
    if let Some(threads) = requested {
        return vec![threads];
    }
    if only == "loads" {
        return vec![native];
    }
    let mut plan = vec![1usize, 2, 4, 8, 16];
    if !plan.contains(&native) {
        plan.push(native);
    }
    plan.sort_unstable();
    plan.dedup();
    plan
}

fn spawn_tick(request: &BenchRequest, vehicles: u64, threads: usize) -> Result<Value, Box<dyn std::error::Error>> {
    eprintln!("benchmark tick {vehicles} vehicles  threads {threads}");
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.current_dir(std::env::current_dir()?)
        .arg("benchmark")
        .arg("--worker")
        .arg("tick")
        .arg("--cache")
        .arg(&request.cache)
        .arg("--threads")
        .arg(threads.to_string())
        .arg("--vehicles")
        .arg(vehicles.to_string())
        .arg("--measure-ticks")
        .arg(request.measure_ticks.to_string())
        .arg("--warmup-s")
        .arg(request.warmup_s.to_string())
        .arg("--seed")
        .arg(request.seed.to_string());
    let mut value = capture_worker(cmd)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("label".into(), json!(load_label(vehicles)));
    }
    if let Some(mean) = value.get("mean_ms").and_then(|v| v.as_f64()) {
        let active = value.get("active_before").and_then(|v| v.as_u64()).unwrap_or(0);
        let rate = value.get("ticks_per_s").and_then(|v| v.as_f64()).unwrap_or(0.0);
        eprintln!("  active {active}  mean {mean:.3} ms  {rate:.1} ticks/s");
    }
    Ok(value)
}

fn spawn_day(request: &BenchRequest, threads: usize) -> Result<Value, Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.current_dir(std::env::current_dir()?)
        .arg("benchmark")
        .arg("--worker")
        .arg("day")
        .arg("--cache")
        .arg(&request.cache)
        .arg("--threads")
        .arg(threads.to_string())
        .arg("--daily-vehicles")
        .arg(request.daily_vehicles.to_string())
        .arg("--horizon-s")
        .arg(request.horizon_s.to_string())
        .arg("--seed")
        .arg(request.seed.to_string());
    capture_worker(cmd)
}

fn capture_worker(mut cmd: Command) -> Result<Value, Box<dyn std::error::Error>> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    let mut child = cmd.spawn()?;
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .ok_or("worker stdout was not piped")?
        .read_to_string(&mut stdout)?;
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("benchmark worker exited {status}\n{stdout}").into());
    }
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with('{'))
        .ok_or_else(|| format!("benchmark worker produced no JSON\n{stdout}"))?;
    Ok(serde_json::from_str(line)?)
}

fn worker_tick(request: &BenchRequest) -> Result<(), Box<dyn std::error::Error>> {
    let threads = request.threads.unwrap_or_else(native_threads).max(1);
    let vehicles = request.vehicles.ok_or("tick worker requires --vehicles")?;
    rayon::ThreadPoolBuilder::new().num_threads(threads).build_global()?;
    let _ = writeln!(std::io::stderr(), "worker tick vehicles {vehicles} threads {threads}");
    let _ = std::io::stderr().flush();
    let loaded = Instant::now();
    let network = Arc::new(load_network(&request.cache)?);
    let load_ms = elapsed_ms(loaded);
    let mut scenario = Scenario::baseline(format!("benchmark-{}", load_label(vehicles)));
    scenario.signal_schedule = SignalSchedule::ForceFixed;
    let prepared = Instant::now();
    let boundary = boundary_demand(&network, &scenario);
    let prepare_ms = elapsed_ms(prepared);
    let mut sim = Simulation::shared(Arc::clone(&network), scenario);
    sim.set_clock_origin(8.0 * 3600.0);
    sim.set_profile_quiet(true);
    let scheduled = schedule_burst(&mut sim, &boundary, vehicles as usize)?;
    let _ = writeln!(
        std::io::stderr(),
        "worker releasing {scheduled} trips on {} edges",
        network.edges.len()
    );
    let _ = std::io::stderr().flush();
    let release_started = Instant::now();
    let searches_before = sim.route_searches();
    sim.step(1.0)?;
    let release_ms = elapsed_ms(release_started);
    let release_searches = sim.route_searches().saturating_sub(searches_before);
    let _ = writeln!(std::io::stderr(), "worker release {release_ms:.1} ms  searches {release_searches}");
    sim.reset_timings();
    let warmup_started = Instant::now();
    advance_to(&mut sim, request.warmup_s)?;
    let warmup_ms = elapsed_ms(warmup_started);
    let before = sim.census();
    sim.reset_timings();
    let searches_mark = sim.route_searches();
    let reroute_mark = sim.reroute_searches();
    let mut samples = Vec::with_capacity(request.measure_ticks as usize);
    let mut alloc_counts = Vec::with_capacity(request.measure_ticks as usize);
    let mut alloc_bytes = 0u64;
    let mut alloc_frees = 0u64;
    let mut zero_alloc_ticks = 0u64;
    let mut max_allocs = 0u64;
    let window_started = Instant::now();
    for _ in 0..request.measure_ticks {
        alloc_count::disarm();
        alloc_count::reset();
        alloc_count::arm();
        let tick_started = Instant::now();
        sim.step(1.0)?;
        let tick_ms = elapsed_ms(tick_started);
        alloc_count::disarm();
        let snap = alloc_count::snapshot();
        samples.push(tick_ms);
        alloc_counts.push(snap.allocs);
        alloc_bytes += snap.bytes;
        alloc_frees += snap.frees;
        max_allocs = max_allocs.max(snap.allocs);
        if snap.allocs == 0 {
            zero_alloc_ticks += 1;
        }
    }
    let wall_ms = elapsed_ms(window_started);
    let after = sim.census();
    let timings = sim.timings();
    let footprint = sim.buffer_footprint();
    let (working, peak, private) = memory_now();
    let publish = bench_publish(&sim);
    let scratch_main = local_router_scratch_bytes();
    let pool = rayon::current_num_threads().max(1);
    let scratch_samples: Vec<u64> = (0..pool).into_par_iter().map(|_| local_router_scratch_bytes()).collect();
    let scratch_sum: u64 = scratch_samples.iter().sum();
    let scratch_max = scratch_samples.iter().copied().max().unwrap_or(0);
    let ticks = samples.len().max(1) as f64;
    let mean_ms = samples.iter().sum::<f64>() / ticks;
    let min_ms = samples.iter().copied().fold(f64::MAX, f64::min);
    let max_ms = samples.iter().copied().fold(0.0, f64::max);
    let mut ordered = samples.clone();
    let p50_ms = percentile(&mut ordered, 0.50);
    let p95_ms = percentile(&mut ordered, 0.95);
    let ticks_per_s = if wall_ms > 0.0 { ticks / (wall_ms / 1e3) } else { 0.0 };
    let routing_ms = ns_to_ms(timings.routing_ns);
    let link_ms = ns_to_ms(timings.link_ns);
    let apply_ms = ns_to_ms(timings.apply_ns);
    let signals_ms = ns_to_ms(timings.signals_ns);
    let recording_ms = ns_to_ms(timings.recording_ns);
    let stage_ms = routing_ms + link_ms + signals_ms + recording_ms;
    let mean_busy_nodes = if timings.busy_ticks == 0 {
        0.0
    } else {
        timings.busy_node_visits as f64 / timings.busy_ticks as f64
    };
    let result = json!({
        "kind": "tick",
        "label": load_label(vehicles),
        "threads": threads,
        "pool_threads": pool,
        "target_vehicles": vehicles,
        "scheduled": scheduled,
        "load_ms": load_ms,
        "prepare_ms": prepare_ms,
        "edges": network.edges.len(),
        "signals": network.signals.len(),
        "release_ms": release_ms,
        "release_searches": release_searches,
        "warmup_s": request.warmup_s,
        "warmup_ms": warmup_ms,
        "sim_time_s": sim.time(),
        "measure_ticks": samples.len(),
        "active_before": before.active,
        "active_after": after.active,
        "on_links": before.on_links,
        "waiting": before.waiting,
        "on_links_after": after.on_links,
        "waiting_after": after.waiting,
        "spawned": after.spawned,
        "completed": after.completed,
        "rejected": sim.rejected_trips(),
        "wall_ms": wall_ms,
        "mean_ms": mean_ms,
        "min_ms": if samples.is_empty() { 0.0 } else { min_ms },
        "p50_ms": p50_ms,
        "p95_ms": p95_ms,
        "max_ms": max_ms,
        "ticks_per_s": ticks_per_s,
        "speedup": ticks_per_s,
        "routing_ms": routing_ms,
        "link_ms": link_ms,
        "apply_ms": apply_ms,
        "signals_ms": signals_ms,
        "recording_ms": recording_ms,
        "stage_ms": stage_ms,
        "unaccounted_ms": wall_ms - stage_ms,
        "routing_pct": pct(routing_ms, stage_ms),
        "link_pct": pct(link_ms, stage_ms),
        "apply_pct": pct(apply_ms, stage_ms),
        "signals_pct": pct(signals_ms, stage_ms),
        "recording_pct": pct(recording_ms, stage_ms),
        "busy_ticks": timings.busy_ticks,
        "mean_busy_nodes": mean_busy_nodes,
        "route_searches_window": sim.route_searches().saturating_sub(searches_mark),
        "reroutes_window": sim.reroute_searches().saturating_sub(reroute_mark),
        "allocs": alloc_counts.iter().sum::<u64>(),
        "alloc_bytes": alloc_bytes,
        "alloc_frees": alloc_frees,
        "allocs_per_tick": alloc_counts.iter().sum::<u64>() as f64 / ticks,
        "max_allocs_per_tick": max_allocs,
        "zero_alloc_ticks": zero_alloc_ticks,
        "rss_bytes": working,
        "peak_rss_bytes": peak,
        "private_bytes": private,
        "footprint": footprint_json(&footprint),
        "router_scratch_main": scratch_main,
        "router_scratch_sum": scratch_sum,
        "router_scratch_max": scratch_max,
        "max_density": sim.max_density(),
        "max_full_edges": sim.max_full_edges(),
        "max_queue": sim.max_queue(),
        "max_spillback": sim.max_spillback(),
        "publish": publish,
    });
    println!("{result}");
    Ok(())
}

fn worker_day(request: &BenchRequest) -> Result<(), Box<dyn std::error::Error>> {
    let threads = request.threads.unwrap_or_else(native_threads).max(1);
    rayon::ThreadPoolBuilder::new().num_threads(threads).build_global()?;
    let _ = writeln!(
        std::io::stderr(),
        "worker day daily {:.0} horizon {:.0}s threads {threads}",
        request.daily_vehicles, request.horizon_s
    );
    let loaded = Instant::now();
    let network = Arc::new(load_network(&request.cache)?);
    let load_ms = elapsed_ms(loaded);
    let scenario = Scenario::baseline("benchmark-24h");
    let profile = DiurnalProfile::urban_default();
    let boundary = boundary_demand(&network, &scenario);
    let zone_scale = gateway_zone_scales(&network, &scenario, &boundary.origins);
    let mut cursor = SeededCursor::default();
    let mut rejected = 0u64;
    let mut sim = Simulation::shared(Arc::clone(&network), scenario.clone());
    sim.set_clock_origin(0.0);
    sim.set_profile_quiet(true);
    let started = Instant::now();
    let mut step_s = 0.0;
    let mut coast_s = 0.0;
    let mut steps = 0u64;
    let mut coasts = 0u64;
    let mut hours = Vec::new();
    let mut peak_active = 0u64;
    let mut peak_on = 0u64;
    let mut peak_wait = 0u64;
    let mut next_hour = 1u32;
    let mut next_sample = 60.0;
    let horizon = request.horizon_s.max(0.0);
    while sim.time() + 1e-6 < horizon {
        note_hours(
            &sim,
            &started,
            &mut hours,
            &mut next_hour,
            &mut peak_active,
            &mut peak_on,
            &mut peak_wait,
            step_s,
            coast_s,
            steps,
        );
        if sim.time() + 1e-6 >= next_sample {
            let census = sim.census();
            peak_active = peak_active.max(census.active);
            peak_on = peak_on.max(census.on_links);
            peak_wait = peak_wait.max(census.waiting);
            next_sample += 60.0;
        }
        let hour = ((sim.time() / 3600.0).floor() as usize) % 24;
        let rate = request.daily_vehicles * scenario.global_demand_scale.max(0.0) * profile.hour_fraction[hour];
        let until = (sim.time() + 1.0).min(horizon);
        let trips = retain_routable(
            emit_seeded_gateway_trips(
                &boundary.origins,
                &boundary.destinations,
                rate,
                request.seed,
                &zone_scale,
                &mut cursor,
                until,
            ),
            &boundary,
            &mut rejected,
        );
        if trips.is_empty() {
            let hour_end = (hour as f64 + 1.0) * 3600.0;
            let jump_to = hour_end.min(horizon).min(cursor.next_s.max(sim.time()));
            let seconds = (jump_to - sim.time()).floor().max(0.0) as u32;
            if seconds >= 1 {
                let coast_started = Instant::now();
                let moved = sim.coast(seconds);
                coast_s += coast_started.elapsed().as_secs_f64();
                if moved > 0 {
                    coasts += 1;
                    continue;
                }
            }
        } else {
            sim.schedule(trips);
        }
        let dt = (until - sim.time()).max(0.0);
        let step_started = Instant::now();
        if dt > 1e-9 {
            sim.step(dt)?;
        } else if sim.time() + 1e-6 < horizon {
            sim.step(1.0)?;
        }
        step_s += step_started.elapsed().as_secs_f64();
        steps += 1;
    }
    note_hours(
        &sim,
        &started,
        &mut hours,
        &mut next_hour,
        &mut peak_active,
        &mut peak_on,
        &mut peak_wait,
        step_s,
        coast_s,
        steps,
    );
    let wall_s = started.elapsed().as_secs_f64();
    let census = sim.census();
    peak_active = peak_active.max(census.active);
    let timings = sim.timings();
    let footprint = sim.buffer_footprint();
    let (working, peak, private) = memory_now();
    let routing_ms = ns_to_ms(timings.routing_ns);
    let link_ms = ns_to_ms(timings.link_ns);
    let apply_ms = ns_to_ms(timings.apply_ns);
    let signals_ms = ns_to_ms(timings.signals_ns);
    let recording_ms = ns_to_ms(timings.recording_ns);
    let stage_ms = routing_ms + link_ms + signals_ms + recording_ms;
    let result = json!({
        "kind": "day",
        "threads": threads,
        "daily_vehicles": request.daily_vehicles,
        "horizon_s": horizon,
        "seed": request.seed,
        "load_ms": load_ms,
        "wall_s": wall_s,
        "step_s": step_s,
        "coast_s": coast_s,
        "steps": steps,
        "coasts": coasts,
        "speedup": if wall_s > 0.0 { horizon / wall_s } else { 0.0 },
        "step_ticks_per_s": if step_s > 0.0 { steps as f64 / step_s } else { 0.0 },
        "spawned": census.spawned,
        "completed": census.completed,
        "active": census.active,
        "on_links": census.on_links,
        "waiting": census.waiting,
        "rejected": rejected,
        "rejected_release": sim.rejected_trips(),
        "peak_active": peak_active,
        "peak_on_links": peak_on,
        "peak_waiting": peak_wait,
        "vht_h": sim.vht_s() / 3600.0,
        "vkt_km": sim.vkt_m() / 1000.0,
        "delay_h": sim.delay_s() / 3600.0,
        "max_density": sim.max_density(),
        "max_full_edges": sim.max_full_edges(),
        "max_queue": sim.max_queue(),
        "max_spillback": sim.max_spillback(),
        "rss_bytes": working,
        "peak_rss_bytes": peak,
        "private_bytes": private,
        "footprint": footprint_json(&footprint),
        "routing_ms": routing_ms,
        "link_ms": link_ms,
        "apply_ms": apply_ms,
        "signals_ms": signals_ms,
        "recording_ms": recording_ms,
        "stage_ms": stage_ms,
        "routing_pct": pct(routing_ms, stage_ms),
        "link_pct": pct(link_ms, stage_ms),
        "apply_pct": pct(apply_ms, stage_ms),
        "signals_pct": pct(signals_ms, stage_ms),
        "recording_pct": pct(recording_ms, stage_ms),
        "busy_ticks": timings.busy_ticks,
        "ticks": timings.ticks,
        "hours": hours,
    });
    println!("{result}");
    Ok(())
}

fn note_hours(
    sim: &Simulation,
    started: &Instant,
    hours: &mut Vec<Value>,
    next_hour: &mut u32,
    peak_active: &mut u64,
    peak_on: &mut u64,
    peak_wait: &mut u64,
    step_s: f64,
    coast_s: f64,
    steps: u64,
) {
    while *next_hour <= 24 && sim.time() + 1e-6 >= f64::from(*next_hour) * 3600.0 {
        let census = sim.census();
        *peak_active = (*peak_active).max(census.active);
        *peak_on = (*peak_on).max(census.on_links);
        *peak_wait = (*peak_wait).max(census.waiting);
        let (working, _, _) = memory_now();
        let hour = *next_hour - 1;
        let _ = writeln!(
            std::io::stderr(),
            "day hour {hour} done  wall {:.1}s  active {}  on {}  waiting {}  rss {:.1} MB",
            started.elapsed().as_secs_f64(),
            census.active,
            census.on_links,
            census.waiting,
            working as f64 / (1024.0 * 1024.0)
        );
        hours.push(json!({
            "hour": hour,
            "sim_time_s": sim.time(),
            "active": census.active,
            "on_links": census.on_links,
            "waiting": census.waiting,
            "rss_bytes": working,
            "wall_s": started.elapsed().as_secs_f64(),
            "step_s": step_s,
            "coast_s": coast_s,
            "steps": steps,
        }));
        *next_hour += 1;
    }
}

fn schedule_burst(
    sim: &mut Simulation,
    boundary: &sim_engine::BoundaryDemand,
    count: usize,
) -> Result<usize, Box<dyn std::error::Error>> {
    let origins = boundary.origins.len();
    let destinations = boundary.destinations.len();
    if origins == 0 || destinations == 0 {
        return Err("the network has no routable gateways".into());
    }
    let mut trips = Vec::with_capacity(count);
    let mut slot = 0usize;
    let limit = count.saturating_mul(16).max(count + 1);
    while trips.len() < count && slot < limit {
        let origin = boundary.origins[slot % origins];
        let destination = boundary.destinations[(slot.wrapping_mul(7).wrapping_add(1)) % destinations];
        slot += 1;
        if origin != destination && boundary.allows(origin, destination) {
            trips.push(Trip::new(0.0, origin, destination));
        }
    }
    if trips.len() < count {
        return Err(format!("scheduled {} routable trips, wanted {count}", trips.len()).into());
    }
    let scheduled = trips.len();
    sim.schedule(trips);
    Ok(scheduled)
}

fn advance_to(sim: &mut Simulation, horizon: f64) -> Result<(), Box<dyn std::error::Error>> {
    let mut spins = 0u32;
    while sim.time() + 1e-6 < horizon {
        spins += 1;
        if spins > 200_000 {
            return Err("warmup did not reach its horizon".into());
        }
        let remaining = (horizon - sim.time()).floor().max(0.0) as u32;
        if remaining >= 1 && sim.coast(remaining) > 0 {
            continue;
        }
        let dt = (horizon - sim.time()).min(1.0);
        if dt <= 1e-9 {
            break;
        }
        sim.step(dt)?;
    }
    Ok(())
}

fn bench_publish(sim: &Simulation) -> Value {
    let edges = sim.network().edges.len();
    let mut speed = vec![0f32; edges];
    let mut density = vec![0f32; edges];
    let mut queue = vec![0f32; edges];
    let mut frame = Vec::new();
    let mut published = Vec::new();
    let census = sim.census();
    let mean = sim.mean_speed_mps();
    sim.write_link_fields(&mut speed, &mut density, &mut queue);
    let ranked = top_bottlenecks(&speed, &queue, 8);
    let header = frame_header(sim, &census, mean);
    encode_frame(&header, &ranked, &speed, &density, &queue, &mut frame);
    published.clone_from(&frame);
    let fields_ms = time_mean(PUBLISH_ITERS, || {
        sim.write_link_fields(&mut speed, &mut density, &mut queue);
        std::hint::black_box(speed[0]);
    });
    let mean_ms = time_mean(PUBLISH_ITERS, || {
        std::hint::black_box(sim.mean_speed_mps());
    });
    let rank_ms = time_mean(PUBLISH_ITERS, || {
        let ranked = top_bottlenecks(&speed, &queue, 8);
        std::hint::black_box(ranked.len());
    });
    let encode_ms = time_mean(PUBLISH_ITERS, || {
        encode_frame(&header, &ranked, &speed, &density, &queue, &mut frame);
        std::hint::black_box(frame.len());
    });
    let publish_copy_ms = time_mean(PUBLISH_ITERS, || {
        published.clone_from(&frame);
        std::hint::black_box(published.len());
    });
    let invoke_copy_ms = time_mean(PUBLISH_ITERS, || {
        let copy = frame.clone();
        std::hint::black_box(copy.len());
    });
    let decode_ms = time_mean(PUBLISH_ITERS, || {
        let decoded = decode_frame(&frame).expect("frame");
        std::hint::black_box(decoded.header.active);
    });
    let combined_started = Instant::now();
    for _ in 0..PUBLISH_ITERS {
        sim.write_link_fields(&mut speed, &mut density, &mut queue);
        let mean = sim.mean_speed_mps();
        let census = sim.census();
        let ranked = top_bottlenecks(&speed, &queue, 8);
        let header = frame_header(sim, &census, mean);
        encode_frame(&header, &ranked, &speed, &density, &queue, &mut frame);
        published.clone_from(&frame);
        let copy = frame.clone();
        std::hint::black_box(copy.len());
    }
    let combined_ms = elapsed_ms(combined_started) / f64::from(PUBLISH_ITERS);
    json!({
        "edges": edges,
        "frame_bytes": frame.len(),
        "bottlenecks": ranked.len(),
        "iterations": PUBLISH_ITERS,
        "fields_ms": fields_ms,
        "mean_speed_ms": mean_ms,
        "rank_ms": rank_ms,
        "encode_ms": encode_ms,
        "publish_copy_ms": publish_copy_ms,
        "invoke_copy_ms": invoke_copy_ms,
        "decode_ms": decode_ms,
        "combined_ms": combined_ms,
    })
}

fn frame_header(sim: &Simulation, census: &sim_engine::Census, mean: f64) -> FrameHeader {
    FrameHeader {
        sequence: 1,
        time_s: sim.time(),
        speed: 1.0,
        paused: false,
        seeking: false,
        mean_speed_mps: mean as f32,
        vht_s: sim.vht_s(),
        vkt_m: sim.vkt_m(),
        delay_s: sim.delay_s(),
        spawned: census.spawned.min(u32::MAX as u64) as u32,
        active: census.active.min(u32::MAX as u64) as u32,
        completed: census.completed.min(u32::MAX as u64) as u32,
    }
}

fn measure_static(cache: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let (rss_before, _, _) = memory_now();
    let started = Instant::now();
    let network = load_network(cache)?;
    let load_ms = elapsed_ms(started);
    let (rss_loaded, _, _) = memory_now();
    let cache_bytes = fs::metadata(cache)?.len();
    let graph = graph_memory(&network);
    let sim_started = Instant::now();
    let sim = Simulation::new(network, Scenario::baseline("benchmark-empty"));
    let empty_sim_ms = elapsed_ms(sim_started);
    let (rss_sim, peak, private) = memory_now();
    let footprint = sim.buffer_footprint();
    let edges = sim.network().edges.len();
    let paths = write_paths(sim.network())?;
    let mut codec = codec_bench(edges);
    let locality = locality_probe(edges);
    let js = js_unpack(&codec);
    if let Some(object) = codec.as_object_mut() {
        object.remove("frame_path");
    }
    Ok(json!({
        "cache_bytes": cache_bytes,
        "load_ms": load_ms,
        "empty_sim_ms": empty_sim_ms,
        "rss_before_bytes": rss_before,
        "rss_loaded_bytes": rss_loaded,
        "rss_sim_bytes": rss_sim,
        "peak_rss_bytes": peak,
        "private_bytes": private,
        "graph": graph,
        "paths": paths,
        "empty_footprint": footprint_json(&footprint),
        "codec": codec,
        "js_unpack": js,
        "locality": locality,
    }))
}

fn write_paths(network: &Network) -> Result<Value, Box<dyn std::error::Error>> {
    let path = PathBuf::from("docs/perf-paths.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = std::io::BufWriter::new(fs::File::create(&path)?);
    write!(file, "{{\"paths\":[")?;
    let mut points = 0usize;
    for (index, edge) in network.edges.iter().enumerate() {
        if index > 0 {
            write!(file, ",")?;
        }
        write!(file, "[")?;
        for (point_index, point) in edge.geometry_lonlat.iter().enumerate() {
            if point_index > 0 {
                write!(file, ",")?;
            }
            write!(file, "[{:.6},{:.6}]", point.lon_deg, point.lat_deg)?;
            points += 1;
        }
        write!(file, "]")?;
    }
    write!(file, "]}}")?;
    file.flush()?;
    let bytes = fs::metadata(&path)?.len();
    Ok(json!({
        "file": path,
        "paths": network.edges.len(),
        "points": points,
        "bytes": bytes,
    }))
}

fn graph_memory(network: &Network) -> Value {
    fn vec_bytes<T>(capacity: usize) -> u64 {
        capacity as u64 * std::mem::size_of::<T>() as u64
    }
    fn text_bytes(value: &Option<String>) -> u64 {
        value.as_ref().map(|text| text.capacity() as u64).unwrap_or(0)
    }
    let mut geometry_xy = 0u64;
    let mut geometry_lonlat = 0u64;
    let mut way_ids = 0u64;
    let mut names = 0u64;
    let mut refs = 0u64;
    let mut storage = 0.0f64;
    for edge in &network.edges {
        geometry_xy += vec_bytes::<Xy>(edge.geometry_xy.capacity());
        geometry_lonlat += vec_bytes::<LonLat>(edge.geometry_lonlat.capacity());
        way_ids += edge.osm_way_ids.capacity() as u64 * 8;
        names += text_bytes(&edge.name);
        refs += text_bytes(&edge.road_ref);
        storage += edge.storage_max_veh;
    }
    let mut phase_bytes = 0u64;
    let mut phase_edge_bytes = 0u64;
    for signal in &network.signals {
        phase_bytes += vec_bytes::<Phase>(signal.phases.capacity());
        for phase in &signal.phases {
            phase_edge_bytes += vec_bytes::<EdgeId>(phase.incoming.capacity());
        }
    }
    let mut zone_bytes = 0u64;
    for zone in &network.zones {
        zone_bytes += vec_bytes::<NodeId>(zone.nodes.capacity());
    }
    let nodes = vec_bytes::<Node>(network.nodes.capacity());
    let edges = vec_bytes::<Edge>(network.edges.capacity());
    let turns = vec_bytes::<Turn>(network.turns.capacity());
    let signals = vec_bytes::<SignalController>(network.signals.capacity());
    let zones = vec_bytes::<Zone>(network.zones.capacity()) + zone_bytes;
    let adjacency = vec_bytes::<u32>(network.adjacency.out_offset.capacity())
        + vec_bytes::<EdgeId>(network.adjacency.out_edges.capacity())
        + vec_bytes::<u32>(network.adjacency.in_offset.capacity())
        + vec_bytes::<EdgeId>(network.adjacency.in_edges.capacity());
    let turn_offset = vec_bytes::<u32>(network.turn_offset.capacity());
    let names_total = names + refs + text_bytes(&network.preset_id) + network.stats.diagram.capacity() as u64;
    let geometry = geometry_xy + geometry_lonlat;
    let total = nodes + edges + geometry + way_ids + names_total + turns + turn_offset + signals + phase_bytes
        + phase_edge_bytes + zones + adjacency + std::mem::size_of::<Network>() as u64;
    let edge_count = network.edges.len().max(1) as u64;
    let node_count = network.nodes.len().max(1) as u64;
    json!({
        "nodes": network.nodes.len(),
        "edges": network.edges.len(),
        "turns": network.turns.len(),
        "signals": network.signals.len(),
        "gateways": network.stats.gateways,
        "zones": network.zones.len(),
        "length_km": network.stats.total_length_m / 1000.0,
        "lane_km": network.stats.total_lane_m / 1000.0,
        "mean_edge_m": network.stats.mean_edge_length_m,
        "storage_veh": storage,
        "diagram": network.stats.diagram,
        "sizeof_node": std::mem::size_of::<Node>(),
        "sizeof_edge": std::mem::size_of::<Edge>(),
        "sizeof_turn": std::mem::size_of::<Turn>(),
        "sizeof_signal": std::mem::size_of::<SignalController>(),
        "sizeof_vehicle": std::mem::size_of::<sim_engine::Vehicle>(),
        "nodes_bytes": nodes,
        "edges_bytes": edges,
        "geometry_bytes": geometry,
        "way_id_bytes": way_ids,
        "name_bytes": names_total,
        "turns_bytes": turns,
        "turn_offset_bytes": turn_offset,
        "adjacency_bytes": adjacency,
        "signals_bytes": signals + phase_bytes + phase_edge_bytes,
        "zones_bytes": zones,
        "total_bytes": total,
        "bytes_per_edge": total as f64 / edge_count as f64,
        "bytes_per_node": total as f64 / node_count as f64,
    })
}

fn codec_bench(edges: usize) -> Value {
    let speed = vec![0.72f32; edges];
    let density = vec![0.15f32; edges];
    let mut queue = vec![0f32; edges];
    if edges > 10 {
        queue[3] = 12.0;
        queue[8] = 4.0;
    }
    let ranked = top_bottlenecks(&speed, &queue, 8);
    let header = FrameHeader {
        sequence: 1,
        time_s: 28_800.0,
        speed: 20.0,
        paused: false,
        seeking: false,
        mean_speed_mps: 11.0,
        vht_s: 100.0,
        vkt_m: 1000.0,
        delay_s: 10.0,
        spawned: 100,
        active: 80,
        completed: 20,
    };
    let mut frame = Vec::new();
    encode_frame(&header, &ranked, &speed, &density, &queue, &mut frame);
    let encode_ms = time_mean(50, || {
        encode_frame(&header, &ranked, &speed, &density, &queue, &mut frame);
        std::hint::black_box(frame.len());
    });
    let decode_ms = time_mean(50, || {
        let decoded = decode_frame(&frame).expect("frame");
        std::hint::black_box(decoded.speed_ratio.len());
    });
    let mut copy = Vec::new();
    let copy_ms = time_mean(50, || {
        copy.clone_from(&frame);
        std::hint::black_box(copy.len());
    });
    let frame_path = std::env::temp_dir().join("traffic-sim-perf-frame.bin");
    let _ = fs::write(&frame_path, &frame);
    json!({
        "edges": edges,
        "frame_bytes": frame.len(),
        "magic": "0x54534631",
        "encode_ms": encode_ms,
        "decode_ms": decode_ms,
        "copy_ms": copy_ms,
        "frame_path": frame_path,
    })
}

fn js_unpack(codec: &Value) -> Value {
    let Some(path) = codec.get("frame_path").and_then(|value| value.as_str()) else {
        return json!({"error": "codec frame was not retained"});
    };
    let script = PathBuf::from("tools/bench-frame-decode.mjs");
    if !script.exists() {
        return json!({"error": "tools/bench-frame-decode.mjs is missing"});
    }
    let output = Command::new("node").arg(&script).arg(path).output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            match serde_json::from_str::<Value>(text.trim()) {
                Ok(value) => value,
                Err(error) => json!({"error": format!("{error}: {text}")}),
            }
        }
        Ok(output) => json!({
            "error": format!(
                "node exited {}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        }),
        Err(error) => json!({"error": format!("node was not run: {error}")}),
    }
}

fn locality_probe(edges: usize) -> Value {
    let n = edges.max(1);
    let sequential = vec![1.0f64; n];
    let seq_ns = time_per_element(n, || {
        let mut sum = 0.0;
        for value in &sequential {
            sum += *value;
        }
        std::hint::black_box(sum);
    });
    let mut queues = Vec::with_capacity(n);
    for index in 0..n {
        let mut queue = std::collections::VecDeque::with_capacity(4);
        queue.push_back(index as u64);
        queues.push(queue);
    }
    let chase_ns = time_per_element(n, || {
        let mut sum = 0u64;
        for queue in &queues {
            sum += queue.front().copied().unwrap_or(0);
        }
        std::hint::black_box(sum);
    });
    let mut order: Vec<usize> = (0..n).collect();
    shuffle(&mut order);
    let gather_ns = time_per_element(n, || {
        let mut sum = 0.0;
        for index in &order {
            sum += sequential[*index];
        }
        std::hint::black_box(sum);
    });
    json!({
        "elements": n,
        "sequential_f64_ns": seq_ns,
        "random_gather_ns": gather_ns,
        "queue_pointer_chase_ns": chase_ns,
        "chase_over_sequential": if seq_ns > 0.0 { chase_ns / seq_ns } else { 0.0 },
        "gather_over_sequential": if seq_ns > 0.0 { gather_ns / seq_ns } else { 0.0 },
    })
}

fn time_per_element(elements: usize, mut body: impl FnMut()) -> f64 {
    body();
    let started = Instant::now();
    let mut reps = 0u32;
    while started.elapsed() < Duration::from_millis(120) && reps < 20_000 {
        body();
        reps += 1;
    }
    let elapsed = started.elapsed().as_secs_f64();
    elapsed * 1e9 / (f64::from(reps.max(1)) * elements.max(1) as f64)
}

fn time_mean(iters: u32, mut body: impl FnMut()) -> f64 {
    body();
    let started = Instant::now();
    for _ in 0..iters {
        body();
    }
    elapsed_ms(started) / f64::from(iters.max(1))
}

fn shuffle(order: &mut [usize]) {
    let mut state = 0x1234_5678_9abc_def0u64;
    for index in (1..order.len()).rev() {
        state = state.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1);
        let swap = (state as usize) % (index + 1);
        order.swap(index, swap);
    }
}

fn footprint_json(footprint: &sim_engine::BufferFootprint) -> Value {
    json!({
        "vehicle_records": footprint.vehicle_records,
        "vehicle_paths": footprint.vehicle_paths,
        "link_queues": footprint.link_queues,
        "waiting": footprint.waiting,
        "edge_state": footprint.edge_state,
        "routing_scratch": footprint.routing_scratch,
        "pending_trips": footprint.pending_trips,
        "total": footprint.total(),
    })
}

fn load_label(vehicles: u64) -> &'static str {
    if vehicles >= 60_000 {
        "gridlock"
    } else if vehicles >= 15_000 {
        "peak"
    } else {
        "low"
    }
}

fn percentile(samples: &mut [f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    let rank = ((samples.len() - 1) as f64 * p).round() as usize;
    samples[rank.min(samples.len() - 1)]
}

fn pct(part: f64, whole: f64) -> f64 {
    if whole <= 0.0 { 0.0 } else { 100.0 * part / whole }
}

fn ns_to_ms(ns: u128) -> f64 {
    ns as f64 / 1e6
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1e3
}

fn native_threads() -> usize {
    std::thread::available_parallelism().map(|count| count.get()).unwrap_or(1)
}

fn write_doc(path: &Path, doc: &Value) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(doc)?)?;
    Ok(())
}

fn host_info() -> Value {
    let (ram, available) = physical_memory();
    let cpu = cpu_info();
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "logical_processors": native_threads(),
        "ram_bytes": ram,
        "available_ram_bytes": available,
        "cpu": cpu.name,
        "cores": cpu.cores,
        "logical": cpu.logical,
        "mhz": cpu.mhz,
        "rustc": rustc_version(),
    })
}

struct CpuInfo {
    name: String,
    cores: u32,
    logical: u32,
    mhz: u32,
}

fn cpu_info() -> CpuInfo {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$p = Get-CimInstance Win32_Processor | Select-Object -First 1; '{0}|{1}|{2}|{3}' -f $p.Name.Trim(), $p.NumberOfCores, $p.NumberOfLogicalProcessors, $p.MaxClockSpeed",
        ])
        .output();
    let Ok(output) = output else {
        return CpuInfo { name: "unknown".into(), cores: 0, logical: 0, mhz: 0 };
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.trim().split('|');
    CpuInfo {
        name: parts.next().unwrap_or("unknown").trim().to_string(),
        cores: parts.next().and_then(|part| part.trim().parse().ok()).unwrap_or(0),
        logical: parts.next().and_then(|part| part.trim().parse().ok()).unwrap_or(0),
        mhz: parts.next().and_then(|part| part.trim().parse().ok()).unwrap_or(0),
    }
}

fn rustc_version() -> String {
    Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn physical_memory() -> (u64, u64) {
    memory_status()
}

#[cfg(windows)]
fn memory_now() -> (u64, u64, u64) {
    use std::ffi::c_void;
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, cb: u32) -> i32;
    }
    unsafe {
        let mut counters = std::mem::zeroed::<Counters>();
        counters.cb = std::mem::size_of::<Counters>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) == 0 {
            return (0, 0, 0);
        }
        (
            counters.working_set_size as u64,
            counters.peak_working_set_size as u64,
            counters.private_usage as u64,
        )
    }
}

#[cfg(not(windows))]
fn memory_now() -> (u64, u64, u64) {
    (0, 0, 0)
}

#[cfg(windows)]
fn memory_status() -> (u64, u64) {
    #[repr(C)]
    struct Status {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
    }
    unsafe {
        let mut status = std::mem::zeroed::<Status>();
        status.length = std::mem::size_of::<Status>() as u32;
        if GlobalMemoryStatusEx(&mut status) == 0 {
            return (0, 0);
        }
        (status.total_phys, status.avail_phys)
    }
}

#[cfg(not(windows))]
fn memory_status() -> (u64, u64) {
    (0, 0)
}
