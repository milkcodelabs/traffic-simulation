//! The simulation owns a background thread. The webview only reads the latest frame
//! and sends small control messages. A slow step never runs on the UI thread.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sim_core::{Network, Scenario, SignalSchedule};
use sim_engine::{
    boundary_demand, compare_edge, emit_gateway_trips, lane_closure_scales, retain_routable,
    BoundaryDemand, DemandCursor, EdgeObservation, Simulation, DIFF_DEGRADED, DIFF_IMPROVED,
    DIFF_NEUTRAL,
};
use sim_ipc::{
    encode_frame, encode_keepalive, top_bottlenecks, Bottleneck, FrameHeader, FRAME_HEADER_BYTES,
};

const VISUAL_INTERVAL_S: f64 = 60.0;
const CHECKPOINT_INTERVAL_S: f64 = 120.0;
const VISUAL_CAP: usize = 120;
const CHECKPOINT_CAP: usize = 24;

struct Published {
    sequence: u64,
    frame: Vec<u8>,
    error: Option<String>,
    diff: Vec<u8>,
    improved: u32,
    degraded: u32,
    neutral: u32,
    baseline_time_s: f64,
    compared_time_s: f64,
    baseline_vht_s: f64,
    baseline_delay_s: f64,
    compared_vht_s: f64,
    compared_delay_s: f64,
    has_baseline: bool,
}

struct Visual {
    time_s: f64,
    frame: Vec<u8>,
}

struct Checkpoint {
    time_s: f64,
    snapshot: sim_engine::SimSnapshot,
    demand: DemandCursor,
}

struct ScenarioPatch {
    speed_scale: Option<f64>,
    demand_scale: Option<f64>,
    lanes_closed: Option<u8>,
}

enum Command {
    Stop,
    Pause(bool),
    Speed(f64),
    Demand(f64),
    Signal(SignalSchedule),
    Seek(f64),
    Scenario(ScenarioPatch),
    CaptureBaseline,
    Restart,
}

pub struct Session {
    tx: Sender<Command>,
    published: Arc<Mutex<Published>>,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    pub fn spawn(network: Network) -> Self {
        let network = Arc::new(network);
        let published = Arc::new(Mutex::new(Published {
            sequence: 0,
            frame: Vec::new(),
            error: None,
            diff: Vec::new(),
            improved: 0,
            degraded: 0,
            neutral: 0,
            baseline_time_s: f64::NAN,
            compared_time_s: 0.0,
            baseline_vht_s: f64::NAN,
            baseline_delay_s: f64::NAN,
            compared_vht_s: f64::NAN,
            compared_delay_s: f64::NAN,
            has_baseline: false,
        }));
        let (tx, rx) = mpsc::channel();
        let shared = Arc::clone(&published);
        let thread = thread::Builder::new()
            .name("traffic-sim".into())
            .spawn(move || run(network, rx, shared))
            .expect("the simulation thread starts");
        Self {
            tx,
            published,
            thread: Some(thread),
        }
    }

    pub fn pause(&self, paused: bool) -> Result<(), String> {
        self.send(Command::Pause(paused))
    }

    pub fn set_speed(&self, multiplier: f64) -> Result<(), String> {
        self.send(Command::Speed(multiplier))
    }

    pub fn set_demand(&self, vehicles_per_hour: f64) -> Result<(), String> {
        self.send(Command::Demand(vehicles_per_hour))
    }

    pub fn set_signal(&self, schedule: SignalSchedule) -> Result<(), String> {
        self.send(Command::Signal(schedule))
    }

    pub fn seek(&self, time_s: f64) -> Result<(), String> {
        self.send(Command::Seek(time_s))
    }

    pub fn apply_scenario(
        &self,
        speed_scale: Option<f64>,
        demand_scale: Option<f64>,
        lanes_closed: Option<u8>,
    ) -> Result<(), String> {
        self.send(Command::Scenario(ScenarioPatch {
            speed_scale,
            demand_scale,
            lanes_closed,
        }))
    }

    pub fn capture_baseline(&self) -> Result<(), String> {
        self.send(Command::CaptureBaseline)
    }

    pub fn restart(&self) -> Result<(), String> {
        self.send(Command::Restart)
    }

    pub fn diff_packet(&self) -> Vec<u8> {
        let slot = self
            .published
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut bytes = Vec::with_capacity(36 + slot.diff.len());
        bytes.extend_from_slice(&0x54534431u32.to_le_bytes());
        bytes.extend_from_slice(&(slot.diff.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&slot.improved.to_le_bytes());
        bytes.extend_from_slice(&slot.degraded.to_le_bytes());
        bytes.extend_from_slice(&slot.neutral.to_le_bytes());
        bytes.extend_from_slice(&slot.baseline_time_s.to_le_bytes());
        bytes.extend_from_slice(&slot.compared_time_s.to_le_bytes());
        bytes.extend_from_slice(&slot.diff);
        bytes.extend_from_slice(&slot.baseline_vht_s.to_le_bytes());
        bytes.extend_from_slice(&slot.baseline_delay_s.to_le_bytes());
        bytes.extend_from_slice(&slot.compared_vht_s.to_le_bytes());
        bytes.extend_from_slice(&slot.compared_delay_s.to_le_bytes());
        bytes
    }

    pub fn frame_since(&self, since: u64) -> Vec<u8> {
        let slot = self
            .published
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot.sequence == 0 || slot.sequence == since || slot.frame.is_empty() {
            Vec::new()
        } else {
            slot.frame.clone()
        }
    }

    pub fn status(&self) -> (bool, Option<String>) {
        let slot = self
            .published
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (slot.sequence > 0, slot.error.clone())
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.tx
            .send(command)
            .map_err(|_| "simulation thread has stopped".to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(network: Arc<Network>, rx: Receiver<Command>, published: Arc<Mutex<Published>>) {
    let mut sim = Simulation::shared(Arc::clone(&network), Scenario::baseline("desktop"));
    sim.set_clock_origin(0.0);
    let boundary = boundary_demand(&network, sim.scenario());
    let mut rejected = 0_u64;
    let edges = network.edges.len();
    let mut demand = DemandCursor::default();
    let mut paused = false;
    let mut speed = 1.0_f64;
    let mut demand_rate = 0.0_f64;
    let mut demand_scale = 1.0_f64;
    let mut baseline: Option<Vec<EdgeObservation>> = None;
    let mut baseline_time_s = f64::NAN;
    let mut last_diff = Instant::now() - Duration::from_secs(1);
    let mut signal = SignalSchedule::Unchanged;
    let mut seeking_until: Option<f64> = None;
    let mut budget = 0.0_f64;
    let mut last_wall = Instant::now();
    let mut last_publish = Instant::now();
    let mut sequence = 0_u64;
    let mut fault: Option<String> = None;
    let mut speed_ratio = vec![1.0_f32; edges];
    let mut density_ratio = vec![0.0_f32; edges];
    let mut queue_veh = vec![0.0_f32; edges];
    let mut frame = Vec::new();
    let mut visuals = Vec::new();
    let mut checkpoints = Vec::new();
    let mut next_visual = VISUAL_INTERVAL_S;
    let mut next_checkpoint = CHECKPOINT_INTERVAL_S;
    let mut announced_pause = false;
    let mut drawn_appearance = u64::MAX;
    let mut cached_mean = 0.0_f32;
    let mut cached_spawned = 0_u32;
    let mut cached_active = 0_u32;
    let mut cached_completed = 0_u32;

    publish_live(
        &sim,
        speed,
        paused,
        false,
        &mut sequence,
        &mut speed_ratio,
        &mut density_ratio,
        &mut queue_veh,
        &mut frame,
        &published,
        &fault,
        &mut drawn_appearance,
        &mut cached_mean,
        &mut cached_spawned,
        &mut cached_active,
        &mut cached_completed,
        true,
    );
    remember(&sim, &demand, &frame, &mut visuals, &mut checkpoints);

    loop {
        if paused && seeking_until.is_none() {
            match rx.recv_timeout(Duration::from_millis(16)) {
                Ok(command) => {
                    if handle(
                        command,
                        &mut sim,
                        &network,
                        &mut paused,
                        &mut speed,
                        &mut demand_rate,
                        &mut signal,
                        &mut demand,
                        &mut seeking_until,
                        &mut last_wall,
                        &mut budget,
                        &visuals,
                        &checkpoints,
                        &mut sequence,
                        &mut frame,
                        &published,
                        &mut demand_scale,
                        &mut baseline,
                        &mut baseline_time_s,
                    ) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        loop {
            match rx.try_recv() {
                Ok(command) => {
                    if handle(
                        command,
                        &mut sim,
                        &network,
                        &mut paused,
                        &mut speed,
                        &mut demand_rate,
                        &mut signal,
                        &mut demand,
                        &mut seeking_until,
                        &mut last_wall,
                        &mut budget,
                        &visuals,
                        &checkpoints,
                        &mut sequence,
                        &mut frame,
                        &published,
                        &mut demand_scale,
                        &mut baseline,
                        &mut baseline_time_s,
                    ) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if let Some(target) = seeking_until {
            let batch_started = Instant::now();
            while sim.time() + 1e-6 < target && batch_started.elapsed() < Duration::from_millis(12)
            {
                let remaining = (target - sim.time()).floor().max(0.0) as u32;
                if remaining == 0 {
                    break;
                }
                let piece = remaining.min(7_200);
                if let Err(error) = advance_clock(
                    &mut sim,
                    &mut demand,
                    demand_rate * demand_scale,
                    &boundary,
                    &mut rejected,
                    piece,
                ) {
                    fault = Some(error.to_string());
                    paused = true;
                    seeking_until = None;
                    break;
                }
            }
            let still_seeking = seeking_until.is_some_and(|goal| sim.time() + 1e-6 < goal);
            if !still_seeking {
                seeking_until = None;
                last_wall = Instant::now();
                budget = 0.0;
            }
            let publish_now = !still_seeking || last_publish.elapsed() >= Duration::from_millis(30);
            if publish_now {
                publish_live(
                    &sim,
                    speed,
                    paused,
                    still_seeking,
                    &mut sequence,
                    &mut speed_ratio,
                    &mut density_ratio,
                    &mut queue_veh,
                    &mut frame,
                    &published,
                    &fault,
                    &mut drawn_appearance,
                    &mut cached_mean,
                    &mut cached_spawned,
                    &mut cached_active,
                    &mut cached_completed,
                    !still_seeking,
                );
                last_publish = Instant::now();
                if !still_seeking {
                    remember(&sim, &demand, &frame, &mut visuals, &mut checkpoints);
                    next_visual = sim.time() + VISUAL_INTERVAL_S;
                    next_checkpoint = sim.time() + CHECKPOINT_INTERVAL_S;
                }
            }
            continue;
        }

        if paused != announced_pause {
            publish_live(
                &sim,
                speed,
                paused,
                false,
                &mut sequence,
                &mut speed_ratio,
                &mut density_ratio,
                &mut queue_veh,
                &mut frame,
                &published,
                &fault,
                &mut drawn_appearance,
                &mut cached_mean,
                &mut cached_spawned,
                &mut cached_active,
                &mut cached_completed,
                false,
            );
            announced_pause = paused;
            last_publish = Instant::now();
        }

        if paused {
            continue;
        }

        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last_wall).as_secs_f64();
        last_wall = now;
        budget += elapsed * speed;
        let cap = (2.0 * speed).max(1.0);
        if budget > cap {
            budget = cap;
        }
        let whole = budget.floor() as u32;
        let mut stepped = false;
        if whole >= 1 {
            let since_publish = last_publish.elapsed().as_secs_f64();
            let room = (0.030 - since_publish).max(0.0);
            let paced = ((room * speed).floor() as u32).max(1);
            let take = whole.min(paced);
            let batch = Instant::now();
            if let Err(error) = advance_clock(
                &mut sim,
                &mut demand,
                demand_rate * demand_scale,
                &boundary,
                &mut rejected,
                take,
            ) {
                fault = Some(error.to_string());
                paused = true;
                budget = 0.0;
            } else {
                budget -= f64::from(take);
                stepped = true;
                let owed = Duration::from_secs_f64(f64::from(take) / speed);
                let spent = batch.elapsed();
                if owed > spent {
                    let until_frame =
                        Duration::from_millis(30).saturating_sub(last_publish.elapsed());
                    let rest = owed.saturating_sub(spent);
                    let pause = rest.min(until_frame);
                    if !pause.is_zero() {
                        thread::sleep(pause);
                    }
                }
            }
        }
        if last_publish.elapsed() >= Duration::from_millis(30) {
            let sample_due = sim.time() + 1e-9 >= next_visual;
            publish_live(
                &sim,
                speed,
                paused,
                false,
                &mut sequence,
                &mut speed_ratio,
                &mut density_ratio,
                &mut queue_veh,
                &mut frame,
                &published,
                &fault,
                &mut drawn_appearance,
                &mut cached_mean,
                &mut cached_spawned,
                &mut cached_active,
                &mut cached_completed,
                sample_due,
            );
            last_publish = Instant::now();
            if sim.time() + 1e-9 >= next_visual {
                visuals.push(Visual {
                    time_s: sim.time(),
                    frame: frame.clone(),
                });
                trim_visuals(&mut visuals);
                next_visual = sim.time() + VISUAL_INTERVAL_S;
            }
            if sim.time() + 1e-9 >= next_checkpoint {
                checkpoints.push(Checkpoint {
                    time_s: sim.time(),
                    snapshot: sim.snapshot(),
                    demand,
                });
                trim_checkpoints(&mut checkpoints);
                next_checkpoint = sim.time() + CHECKPOINT_INTERVAL_S;
            }
        }
        if baseline.is_some() && last_diff.elapsed() >= Duration::from_millis(250) {
            store_diff(&sim, &baseline, baseline_time_s, &published);
            last_diff = Instant::now();
        }
        if !stepped {
            let until_frame = Duration::from_millis(30).saturating_sub(last_publish.elapsed());
            let until_step = if budget < 1.0 {
                Duration::from_secs_f64(((1.0 - budget) / speed).max(0.0))
            } else {
                until_frame
            };
            let pause = until_step.min(until_frame).min(Duration::from_millis(20));
            if !pause.is_zero() {
                thread::sleep(pause);
            }
        }
    }
}

fn advance_clock(
    sim: &mut Simulation,
    demand: &mut DemandCursor,
    rate: f64,
    boundary: &BoundaryDemand,
    rejected: &mut u64,
    seconds: u32,
) -> Result<(), sim_engine::EngineError> {
    if seconds == 0 {
        return Ok(());
    }
    let until = sim.time() + f64::from(seconds);
    let trips = retain_routable(
        emit_gateway_trips(
            &boundary.origins,
            &boundary.destinations,
            rate,
            demand,
            until,
        ),
        boundary,
        rejected,
    );
    if trips.is_empty() && sim.idle_advance(seconds) == seconds {
        return Ok(());
    }
    if !trips.is_empty() {
        sim.schedule(trips);
    }
    for _ in 0..seconds {
        sim.step(1.0)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle(
    command: Command,
    sim: &mut Simulation,
    network: &Arc<Network>,
    paused: &mut bool,
    speed: &mut f64,
    demand_rate: &mut f64,
    signal: &mut SignalSchedule,
    demand: &mut DemandCursor,
    seeking_until: &mut Option<f64>,
    last_wall: &mut Instant,
    budget: &mut f64,
    visuals: &[Visual],
    checkpoints: &[Checkpoint],
    sequence: &mut u64,
    frame: &mut Vec<u8>,
    published: &Mutex<Published>,
    demand_scale: &mut f64,
    baseline: &mut Option<Vec<EdgeObservation>>,
    baseline_time_s: &mut f64,
) -> bool {
    match command {
        Command::Stop => true,
        Command::Pause(value) => {
            if *paused != value {
                *paused = value;
                *last_wall = Instant::now();
                *budget = 0.0;
            }
            false
        }
        Command::Speed(value) => {
            *speed = value.clamp(1.0, 100.0);
            false
        }
        Command::Demand(value) => {
            *demand_rate = if value.is_finite() {
                value.clamp(0.0, 100_000.0)
            } else {
                0.0
            };
            false
        }
        Command::Signal(schedule) => {
            *signal = schedule;
            sim.scenario_mut().signal_schedule = schedule;
            false
        }
        Command::Seek(target) => {
            let target = if target.is_finite() {
                target.clamp(0.0, 86_400.0)
            } else {
                0.0
            };
            if target + 0.5 < sim.time() {
                if let Some(visual) = nearest_visual(visuals, target) {
                    publish_stored(visual, *speed, *paused, true, sequence, frame, published);
                }
                if let Some(index) = checkpoints
                    .iter()
                    .rposition(|item| item.time_s <= target + 1e-3)
                {
                    let snapshot = checkpoints[index].snapshot.clone();
                    *demand = checkpoints[index].demand;
                    *sim = Simulation::restore(Arc::clone(network), snapshot);
                    sim.scenario_mut().signal_schedule = *signal;
                }
            }
            *seeking_until = Some(target);
            *last_wall = Instant::now();
            *budget = 0.0;
            false
        }
        Command::Scenario(patch) => {
            if let Some(value) = patch.speed_scale {
                sim.scenario_mut().global_speed_scale = if value.is_finite() {
                    value.clamp(0.2, 1.5)
                } else {
                    1.0
                };
                sim.recompute_free_flow();
                sim.invalidate_picture();
            }
            if let Some(value) = patch.demand_scale {
                *demand_scale = if value.is_finite() {
                    value.clamp(0.0, 5.0)
                } else {
                    1.0
                };
                sim.scenario_mut().global_demand_scale = *demand_scale;
            }
            if let Some(lanes) = patch.lanes_closed {
                let lanes = lanes.min(4);
                sim.scenario_mut().edge_capacity_scale = lane_closure_scales(network, lanes);
                sim.recompute_free_flow();
                sim.invalidate_picture();
            }
            false
        }
        Command::CaptureBaseline => {
            *baseline = Some(sim.observations());
            *baseline_time_s = sim.time();
            store_diff(sim, baseline, *baseline_time_s, published);
            {
                let mut slot = published.lock().unwrap_or_else(|error| error.into_inner());
                slot.baseline_vht_s = sim.vht_s();
                slot.baseline_delay_s = sim.delay_s();
            }
            false
        }
        Command::Restart => {
            let scenario = sim.scenario().clone();
            *sim = Simulation::shared(Arc::clone(network), scenario);
            sim.set_clock_origin(0.0);
            *demand = DemandCursor::default();
            *seeking_until = None;
            *budget = 0.0;
            *last_wall = Instant::now();
            sim.invalidate_picture();
            store_diff(sim, baseline, *baseline_time_s, published);
            false
        }
    }
}

fn store_diff(
    sim: &Simulation,
    baseline: &Option<Vec<EdgeObservation>>,
    baseline_time_s: f64,
    published: &Mutex<Published>,
) {
    let edges = sim.network().edges.len();
    let mut classes = vec![DIFF_NEUTRAL; edges];
    let mut improved = 0_u32;
    let mut degraded = 0_u32;
    let mut neutral = 0_u32;
    if let Some(base) = baseline {
        let current = sim.observations();
        for index in 0..edges {
            let free = sim.free_flow_mps(index).max(1.0);
            let test = current[index];
            let before = base.get(index).copied().unwrap_or(EdgeObservation {
                delay_s: 0.0,
                mean_speed_mps: f64::NAN,
            });
            let class = compare_edge(before, test, free);
            classes[index] = class;
            match class {
                DIFF_IMPROVED => improved += 1,
                DIFF_DEGRADED => degraded += 1,
                _ => neutral += 1,
            }
        }
    } else {
        neutral = edges as u32;
    }
    let mut slot = published.lock().unwrap_or_else(|error| error.into_inner());
    slot.diff = classes;
    slot.improved = improved;
    slot.degraded = degraded;
    slot.neutral = neutral;
    slot.baseline_time_s = baseline_time_s;
    slot.compared_time_s = sim.time();
    slot.compared_vht_s = sim.vht_s();
    slot.compared_delay_s = sim.delay_s();
    slot.has_baseline = baseline.is_some();
}

fn nearest_visual(visuals: &[Visual], target: f64) -> Option<&Visual> {
    visuals.iter().min_by(|left, right| {
        (left.time_s - target)
            .abs()
            .total_cmp(&(right.time_s - target).abs())
            .then_with(|| left.time_s.total_cmp(&right.time_s))
    })
}

fn publish_stored(
    visual: &Visual,
    speed: f64,
    paused: bool,
    seeking: bool,
    sequence: &mut u64,
    frame: &mut Vec<u8>,
    published: &Mutex<Published>,
) {
    frame.clone_from(&visual.frame);
    if frame.len() < FRAME_HEADER_BYTES {
        return;
    }
    *sequence += 1;
    frame[8..16].copy_from_slice(&sequence.to_le_bytes());
    frame[24..28].copy_from_slice(&(speed as f32).to_le_bytes());
    let mut flags = 0_u32;
    if paused {
        flags |= 1;
    }
    if seeking {
        flags |= 2;
    }
    frame[28..32].copy_from_slice(&flags.to_le_bytes());
    let mut slot = published.lock().unwrap_or_else(|error| error.into_inner());
    slot.sequence = *sequence;
    slot.frame.clone_from(frame);
}

#[allow(clippy::too_many_arguments)]
fn publish_live(
    sim: &Simulation,
    speed: f64,
    paused: bool,
    seeking: bool,
    sequence: &mut u64,
    speed_ratio: &mut [f32],
    density_ratio: &mut [f32],
    queue_veh: &mut [f32],
    frame: &mut Vec<u8>,
    published: &Mutex<Published>,
    fault: &Option<String>,
    drawn_appearance: &mut u64,
    cached_mean: &mut f32,
    cached_spawned: &mut u32,
    cached_active: &mut u32,
    cached_completed: &mut u32,
    force_full: bool,
) {
    let appearance = sim.appearance();
    let full = force_full || *drawn_appearance != appearance;
    *sequence += 1;
    if full {
        sim.write_link_fields(speed_ratio, density_ratio, queue_veh);
        *cached_mean = sim.mean_speed_mps() as f32;
        *drawn_appearance = appearance;
        let census = sim.census();
        *cached_spawned = sat_u32(census.spawned);
        *cached_active = sat_u32(census.active);
        *cached_completed = sat_u32(census.completed);
    }
    let header = FrameHeader {
        sequence: *sequence,
        time_s: sim.time(),
        speed: speed as f32,
        paused,
        seeking,
        mean_speed_mps: *cached_mean,
        vht_s: sim.vht_s(),
        vkt_m: sim.vkt_m(),
        delay_s: sim.delay_s(),
        spawned: *cached_spawned,
        active: *cached_active,
        completed: *cached_completed,
    };
    if full {
        let ranked: Vec<Bottleneck> = top_bottlenecks(speed_ratio, queue_veh, 8);
        encode_frame(
            &header,
            &ranked,
            speed_ratio,
            density_ratio,
            queue_veh,
            frame,
        );
    } else {
        encode_keepalive(&header, frame);
    }
    let mut slot = published.lock().unwrap_or_else(|error| error.into_inner());
    slot.sequence = *sequence;
    slot.frame.clone_from(frame);
    slot.error.clone_from(fault);
}

fn remember(
    sim: &Simulation,
    demand: &DemandCursor,
    frame: &[u8],
    visuals: &mut Vec<Visual>,
    checkpoints: &mut Vec<Checkpoint>,
) {
    visuals.push(Visual {
        time_s: sim.time(),
        frame: frame.to_vec(),
    });
    trim_visuals(visuals);
    checkpoints.push(Checkpoint {
        time_s: sim.time(),
        snapshot: sim.snapshot(),
        demand: *demand,
    });
    trim_checkpoints(checkpoints);
}

fn trim_visuals(visuals: &mut Vec<Visual>) {
    while visuals.len() > VISUAL_CAP && visuals.len() > 1 {
        visuals.remove(1);
    }
}

fn trim_checkpoints(checkpoints: &mut Vec<Checkpoint>) {
    while checkpoints.len() > CHECKPOINT_CAP && checkpoints.len() > 1 {
        checkpoints.remove(1);
    }
}

fn sat_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
