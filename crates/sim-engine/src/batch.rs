//! Paired Monte Carlo runs of the same boundary demand under two scenarios.
//!
//! The demand is the diurnal boundary instrument on compiled gateway nodes.
//! Seeds change headway jitter and destination pairing. They do not create an
//! origin-destination matrix.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use sim_core::{DiurnalProfile, Network, Scenario, SignalSchedule, TimeWindow};

use crate::{
    boundary_demand, emit_seeded_gateway_trips, gateway_zone_scales, retain_routable,
    EdgeObservation, EngineError, SeededCursor, Simulation,
};

#[derive(Clone, Debug)]
pub struct ExperimentConfig {
    pub daily_vehicles: f64,
    pub horizon_s: f64,
    pub runs: usize,
    pub seed: u64,
}

impl Default for ExperimentConfig {
    fn default() -> Self {
        Self {
            daily_vehicles: 6_000.0,
            horizon_s: 86_400.0,
            runs: 10,
            seed: 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunTotals {
    pub seed: u64,
    pub vht_s: f64,
    pub vkt_m: f64,
    pub delay_s: f64,
    pub spawned: u64,
    pub completed: u64,
}

#[derive(Clone, Debug)]
pub struct ArmOutput {
    pub name: String,
    pub runs: Vec<RunTotals>,
    pub mean_delay_s: Vec<f64>,
    pub mean_speed_mps: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct ExperimentOutput {
    pub baseline: ArmOutput,
    pub intervention: ArmOutput,
}

pub fn baseline_day_scenario() -> Scenario {
    let mut scenario = Scenario::baseline("baseline-24h-signals");
    scenario.signal_schedule = SignalSchedule::ForceFixed;
    scenario
}

pub fn night_flashing_scenario() -> Scenario {
    let mut scenario = Scenario::baseline("night-flashing");
    scenario.signal_schedule = SignalSchedule::Unchanged;
    scenario.night_flashing = Some(TimeWindow::NIGHT_FLASHING);
    scenario
}

/// One seeded horizon under a single scenario. The paired experiment calls the same step.
pub fn run_scenario(
    network: Arc<Network>,
    scenario: &Scenario,
    seed: u64,
    daily_vehicles: f64,
    horizon_s: f64,
) -> Result<RunTotals, EngineError> {
    let (totals, _) = simulate(&network, scenario, seed, daily_vehicles, horizon_s)?;
    Ok(totals)
}

pub fn run_signal_experiment(
    network: Arc<Network>,
    config: &ExperimentConfig,
) -> Result<ExperimentOutput, EngineError> {
    let arms = [baseline_day_scenario(), night_flashing_scenario()];
    let mut jobs = Vec::with_capacity(arms.len() * config.runs.max(1));
    for (arm, scenario) in arms.iter().enumerate() {
        for run in 0..config.runs.max(1) {
            jobs.push(Job {
                arm,
                scenario: scenario.clone(),
                seed: config
                    .seed
                    .wrapping_add((arm as u64) << 32)
                    .wrapping_add(run as u64),
            });
        }
    }
    let daily = config.daily_vehicles.max(0.0);
    let horizon = config.horizon_s.max(0.0);
    let network_ref = &network;
    let finished_jobs = AtomicUsize::new(0);
    let job_count = jobs.len();
    let finished: Vec<Result<Finished, EngineError>> = jobs
        .par_iter()
        .map(|job| {
            let (totals, observations) = simulate(network_ref, &job.scenario, job.seed, daily, horizon)?;
            let done = finished_jobs.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "run {done}/{job_count} arm {} seed {}  vht {:.2} h  vkt {:.1} km  delay {:.2} h  spawned {}",
                job.arm,
                job.seed,
                totals.vht_s / 3600.0,
                totals.vkt_m / 1000.0,
                totals.delay_s / 3600.0,
                totals.spawned
            );
            Ok(Finished {
                arm: job.arm,
                totals,
                observations,
            })
        })
        .collect();
    let finished = finished.into_iter().collect::<Result<Vec<_>, _>>()?;
    let edges = network.edges.len();
    let mut baseline_runs = Vec::new();
    let mut intervention_runs = Vec::new();
    let mut baseline_fold = vec![LinkFold::default(); edges];
    let mut intervention_fold = vec![LinkFold::default(); edges];
    for item in finished {
        let fold = if item.arm == 0 {
            baseline_runs.push(item.totals);
            &mut baseline_fold
        } else {
            intervention_runs.push(item.totals);
            &mut intervention_fold
        };
        for (index, observation) in item.observations.into_iter().enumerate() {
            if index >= fold.len() {
                break;
            }
            fold[index].delay += observation.delay_s;
            fold[index].n += 1;
            if observation.observed() {
                fold[index].speed += observation.mean_speed_mps;
                fold[index].speed_n += 1;
            }
        }
    }
    Ok(ExperimentOutput {
        baseline: ArmOutput {
            name: arms[0].name.clone(),
            runs: baseline_runs,
            mean_delay_s: means_delay(&baseline_fold),
            mean_speed_mps: means_speed(&baseline_fold),
        },
        intervention: ArmOutput {
            name: arms[1].name.clone(),
            runs: intervention_runs,
            mean_delay_s: means_delay(&intervention_fold),
            mean_speed_mps: means_speed(&intervention_fold),
        },
    })
}

struct Job {
    arm: usize,
    scenario: Scenario,
    seed: u64,
}

struct Finished {
    arm: usize,
    totals: RunTotals,
    observations: Vec<EdgeObservation>,
}

#[derive(Clone, Copy, Default)]
struct LinkFold {
    delay: f64,
    speed: f64,
    speed_n: u32,
    n: u32,
}

fn means_delay(fold: &[LinkFold]) -> Vec<f64> {
    fold.iter()
        .map(|link| {
            if link.n == 0 {
                0.0
            } else {
                link.delay / f64::from(link.n)
            }
        })
        .collect()
}

fn means_speed(fold: &[LinkFold]) -> Vec<f64> {
    fold.iter()
        .map(|link| {
            if link.speed_n == 0 {
                f64::NAN
            } else {
                link.speed / f64::from(link.speed_n)
            }
        })
        .collect()
}

fn simulate(
    network: &Arc<Network>,
    scenario: &Scenario,
    seed: u64,
    daily_vehicles: f64,
    horizon_s: f64,
) -> Result<(RunTotals, Vec<EdgeObservation>), EngineError> {
    let profile = DiurnalProfile::urban_default();
    let boundary = boundary_demand(network, scenario);
    let zone_scale = gateway_zone_scales(network, scenario, &boundary.origins);
    let mut cursor = SeededCursor::default();
    let mut rejected = 0_u64;
    let mut sim = Simulation::shared(Arc::clone(network), scenario.clone());
    sim.set_clock_origin(0.0);
    let mut reported_hour = 0u32;
    let mut searches_mark = 0u64;
    let mut reroutes_mark = 0u64;
    let mut hour_started = Instant::now();
    let mut coast_s = 0.0;
    let mut step_s = 0.0;
    let mut coasts = 0u64;
    let mut steps = 0u64;
    while sim.time() + 1e-6 < horizon_s {
        let hour_index = (sim.time() / 3600.0) as u32;
        if hour_index > reported_hour {
            let searches = sim.route_searches();
            let reroutes = sim.reroute_searches();
            let mut empty = 0u64;
            let mut waiting = 0u64;
            let mut moving = 0u64;
            for vehicle in sim.vehicles() {
                if vehicle.arrive_s.is_some() {
                    continue;
                }
                if vehicle.path.is_empty() {
                    empty += 1;
                }
                if vehicle.on_link {
                    moving += 1;
                } else {
                    waiting += 1;
                }
            }
            eprintln!(
                "seed {seed}  hour {} done  {:.2}s  searches {}  reroutes {}  rejected {}  empty {}  waiting {}  moving {}  max_density {:.3}  max_queue {}  full {}  spillback {}",
                hour_index - 1,
                hour_started.elapsed().as_secs_f64(),
                searches - searches_mark,
                reroutes - reroutes_mark,
                rejected,
                empty,
                waiting,
                moving,
                sim.max_density(),
                sim.max_queue(),
                sim.max_full_edges(),
                sim.max_spillback(),
            );
            searches_mark = searches;
            reroutes_mark = reroutes;
            reported_hour = hour_index;
            hour_started = Instant::now();
        }
        let hour = ((sim.time() / 3600.0).floor() as usize) % 24;
        let rate =
            daily_vehicles * scenario.global_demand_scale.max(0.0) * profile.hour_fraction[hour];
        let until = (sim.time() + 1.0).min(horizon_s);
        let trips = retain_routable(
            emit_seeded_gateway_trips(
                &boundary.origins,
                &boundary.destinations,
                rate,
                seed,
                &zone_scale,
                &mut cursor,
                until,
            ),
            &boundary,
            &mut rejected,
        );
        if trips.is_empty() {
            let hour_end = (hour as f64 + 1.0) * 3600.0;
            let jump_to = hour_end.min(horizon_s).min(cursor.next_s.max(sim.time()));
            let seconds = (jump_to - sim.time()).floor().max(0.0) as u32;
            if seconds >= 1 {
                let started = Instant::now();
                let moved = sim.coast(seconds);
                coast_s += started.elapsed().as_secs_f64();
                if moved > 0 {
                    coasts += 1;
                    continue;
                }
            }
        } else {
            sim.schedule(trips);
        }
        let dt = (until - sim.time()).max(0.0);
        let started = Instant::now();
        if dt > 1e-9 {
            sim.step(dt)?;
        } else if sim.time() + 1e-6 < horizon_s {
            sim.step(1.0)?;
        }
        step_s += started.elapsed().as_secs_f64();
        steps += 1;
    }
    let speed_kmh = if sim.vht_s() > 1.0 {
        sim.vkt_m() * 3.6 / sim.vht_s()
    } else {
        0.0
    };
    eprintln!(
        "seed {seed} done  {steps} steps in {step_s:.1}s  {rate:.0} ticks/s  {coasts} coasts in {coast_s:.1}s  rejected {rejected}  speed {speed_kmh:.1} km/h",
        rate = steps as f64 / step_s.max(1e-9)
    );
    let census = sim.census();
    if census.spawned != census.active + census.completed {
        return Err(EngineError::Conservation {
            spawned: census.spawned,
            active: census.active,
            completed: census.completed,
        });
    }
    for (what, value) in [
        ("vht", sim.vht_s()),
        ("vkt", sim.vkt_m()),
        ("delay", sim.delay_s()),
        ("mean_speed", sim.mean_speed_mps()),
    ] {
        if !value.is_finite() {
            return Err(EngineError::NonFinite { what });
        }
    }
    eprintln!(
        "conservation spawned {} active {} completed {} on_links {} waiting {}  saturation max_density {:.3} max_queue {} full_edges {} spillback {}",
        census.spawned,
        census.active,
        census.completed,
        census.on_links,
        census.waiting,
        sim.max_density(),
        sim.max_queue(),
        sim.max_full_edges(),
        sim.max_spillback(),
    );
    let totals = RunTotals {
        seed,
        vht_s: sim.vht_s(),
        vkt_m: sim.vkt_m(),
        delay_s: sim.delay_s(),
        spawned: census.spawned,
        completed: census.completed,
    };
    Ok((totals, sim.observations()))
}
