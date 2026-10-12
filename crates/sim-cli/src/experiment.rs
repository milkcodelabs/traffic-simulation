use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use sim_core::load_network;
use sim_engine::{
    compare_edge, distribution, run_signal_experiment, EdgeObservation, ExperimentConfig, DIFF_DEGRADED, DIFF_IMPROVED,
    DIFF_NEUTRAL,
};

pub fn run_headless(
    cache: &Path,
    output: &Path,
    runs: usize,
    daily_vehicles: f64,
    horizon_s: f64,
    seed: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    eprintln!("loading {}", cache.display());
    let network = Arc::new(load_network(cache)?);
    eprintln!(
        "network edges {}  signals {}  gateways {}",
        network.edges.len(),
        network.signals.len(),
        network.stats.gateways
    );
    let config = ExperimentConfig {
        daily_vehicles,
        horizon_s,
        runs,
        seed,
    };
    eprintln!(
        "headless paired runs {runs}  horizon {horizon_s:.0}s  daily boundary vehicles {daily_vehicles:.0}"
    );
    let report = run_signal_experiment(network, &config)?;
    fs::create_dir_all(output)?;
    write_summary(output, &report)?;
    write_runs(output, &report)?;
    write_links(output, &report)?;
    write_diff(output, &report)?;
    fs::write(
        output.join("scenarios.json"),
        "{\n  \"baseline\": {\"name\": \"baseline-24h-signals\", \"signalSchedule\": \"forceFixed\"},\n  \"intervention\": {\"name\": \"night-flashing\", \"signalSchedule\": \"unchanged\", \"nightFlashing\": {\"startS\": 0, \"endS\": 21600}},\n  \"demand\": \"diurnal boundary vehicles at compiled gateways, not an origin-destination matrix\"\n}\n",
    )?;
    eprintln!("wrote {} in {:.1}s", output.display(), started.elapsed().as_secs_f64());
    Ok(())
}

fn write_summary(output: &Path, report: &sim_engine::ExperimentOutput) -> Result<(), Box<dyn std::error::Error>> {
    let mut csv = String::from("arm,metric,n,mean,variance,ci95_low,ci95_high\n");
    for arm in [&report.baseline, &report.intervention] {
        push_metric(&mut csv, &arm.name, "vht_h", &arm.runs.iter().map(|run| run.vht_s / 3600.0).collect::<Vec<_>>());
        push_metric(&mut csv, &arm.name, "vkt_km", &arm.runs.iter().map(|run| run.vkt_m / 1000.0).collect::<Vec<_>>());
        push_metric(&mut csv, &arm.name, "delay_h", &arm.runs.iter().map(|run| run.delay_s / 3600.0).collect::<Vec<_>>());
    }
    fs::write(output.join("summary.csv"), csv)?;
    Ok(())
}

fn push_metric(csv: &mut String, arm: &str, metric: &str, samples: &[f64]) {
    let summary = distribution(samples);
    csv.push_str(&format!(
        "{arm},{metric},{},{:.6},{:.6},{:.6},{:.6}\n",
        summary.n, summary.mean, summary.variance, summary.ci95_low, summary.ci95_high
    ));
}

fn write_runs(output: &Path, report: &sim_engine::ExperimentOutput) -> Result<(), Box<dyn std::error::Error>> {
    let mut csv = String::from("arm,seed,vht_s,vkt_m,delay_s,spawned,completed\n");
    for arm in [&report.baseline, &report.intervention] {
        for run in &arm.runs {
            csv.push_str(&format!(
                "{},{},{:.3},{:.3},{:.3},{},{}\n",
                arm.name, run.seed, run.vht_s, run.vkt_m, run.delay_s, run.spawned, run.completed
            ));
        }
    }
    fs::write(output.join("runs.csv"), csv)?;
    Ok(())
}

fn write_links(output: &Path, report: &sim_engine::ExperimentOutput) -> Result<(), Box<dyn std::error::Error>> {
    let mut csv = String::from("edge,base_delay_s,test_delay_s,delta_delay_s,base_speed_mps,test_speed_mps,delta_speed_mps,class\n");
    let edges = report.baseline.mean_delay_s.len();
    for index in 0..edges {
        let base = observation(report.baseline.mean_delay_s[index], report.baseline.mean_speed_mps[index]);
        let test = observation(report.intervention.mean_delay_s[index], report.intervention.mean_speed_mps[index]);
        let class = compare_edge(base, test, finite_or(base.mean_speed_mps, test.mean_speed_mps));
        let base_speed = format_speed(base.mean_speed_mps);
        let test_speed = format_speed(test.mean_speed_mps);
        let delta_speed = if base.observed() && test.observed() {
            format!("{:.4}", test.mean_speed_mps - base.mean_speed_mps)
        } else {
            String::new()
        };
        csv.push_str(&format!(
            "{index},{:.4},{:.4},{:.4},{base_speed},{test_speed},{delta_speed},{class}\n",
            base.delay_s,
            test.delay_s,
            test.delay_s - base.delay_s
        ));
    }
    fs::write(output.join("links.csv"), csv)?;
    Ok(())
}

fn write_diff(output: &Path, report: &sim_engine::ExperimentOutput) -> Result<(), Box<dyn std::error::Error>> {
    let edges = report.baseline.mean_delay_s.len();
    let mut classes = Vec::with_capacity(edges);
    let mut improved = 0_u32;
    let mut degraded = 0_u32;
    let mut neutral = 0_u32;
    for index in 0..edges {
        let base = observation(report.baseline.mean_delay_s[index], report.baseline.mean_speed_mps[index]);
        let test = observation(report.intervention.mean_delay_s[index], report.intervention.mean_speed_mps[index]);
        let class = compare_edge(base, test, finite_or(base.mean_speed_mps, test.mean_speed_mps));
        match class {
            DIFF_IMPROVED => improved += 1,
            DIFF_DEGRADED => degraded += 1,
            DIFF_NEUTRAL => neutral += 1,
            _ => neutral += 1,
        }
        classes.push(class);
    }
    let mut bytes = Vec::with_capacity(36 + classes.len());
    bytes.extend_from_slice(&0x54534431u32.to_le_bytes());
    bytes.extend_from_slice(&(edges as u32).to_le_bytes());
    bytes.extend_from_slice(&improved.to_le_bytes());
    bytes.extend_from_slice(&degraded.to_le_bytes());
    bytes.extend_from_slice(&neutral.to_le_bytes());
    bytes.extend_from_slice(&f64::NAN.to_le_bytes());
    bytes.extend_from_slice(&0.0_f64.to_le_bytes());
    bytes.extend_from_slice(&classes);
    fs::write(output.join("diff.bin"), bytes)?;
    eprintln!("diff improved {improved}  degraded {degraded}  neutral {neutral}");
    Ok(())
}

fn observation(delay_s: f64, mean_speed_mps: f64) -> EdgeObservation {
    EdgeObservation { delay_s, mean_speed_mps }
}

fn format_speed(speed: f64) -> String {
    if speed.is_finite() {
        format!("{speed:.4}")
    } else {
        String::new()
    }
}

fn finite_or(left: f64, right: f64) -> f64 {
    if left.is_finite() && left > 1.0 {
        left
    } else if right.is_finite() && right > 1.0 {
        right
    } else {
        10.0
    }
}
