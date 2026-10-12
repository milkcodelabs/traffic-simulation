#![recursion_limit = "512"]

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use std::path::Path;

use sim_core::{load_network, BBox};
use sim_ingest::{default_data_dir, run, IngestRequest, IngestSource};

mod alloc_count;
mod audit;
mod benchmark;
mod experiment;

#[global_allocator]
static GLOBAL: alloc_count::CountingAlloc = alloc_count::CountingAlloc;

#[derive(Parser)]
#[command(
    name = "sim-cli",
    about = "Compile a road network and run headless scenario experiments."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download or read a road network and write a .simnet cache.
    Ingest {
        #[arg(long, conflicts_with_all = ["osm", "bbox"])]
        preset: Option<String>,
        #[arg(long, conflicts_with_all = ["preset", "bbox"])]
        osm: Option<PathBuf>,
        /// min_lat,min_lon,max_lat,max_lon
        #[arg(long, conflicts_with_all = ["preset", "osm"])]
        bbox: Option<String>,
        #[arg(long)]
        cache_dir: Option<PathBuf>,
        #[arg(long)]
        refresh: bool,
    },
    /// Print the stats stored in a cache.
    #[command(visible_alias = "summary")]
    Inspect {
        #[arg(long)]
        cache: PathBuf,
    },
    /// Advance one scenario over a horizon and print network totals.
    Run {
        #[arg(long)]
        cache: Option<PathBuf>,
        /// Named city cache. `thessaloniki` loads `data/cache/thessaloniki.simnet`.
        #[arg(long, default_value = "thessaloniki")]
        preset: String,
        /// Scenario JSON. See `data/presets/`.
        #[arg(long)]
        scenario: PathBuf,
        #[arg(long, default_value_t = 6000.0)]
        daily_vehicles: f64,
        #[arg(long, default_value_t = 86400.0)]
        horizon_s: f64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Directory for `run.csv`. Omit to print totals only.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Compare baseline fixed signals with night flashing amber.
    #[command(visible_alias = "batch")]
    Experiment {
        /// Run the paired Monte Carlo batch on the CPU pool.
        #[arg(long)]
        headless: bool,
        #[arg(long, default_value_t = 10)]
        runs: usize,
        #[arg(long)]
        cache: Option<PathBuf>,
        /// Named city cache. `thessaloniki` loads `data/cache/thessaloniki.simnet`.
        #[arg(long, default_value = "thessaloniki")]
        preset: String,
        #[arg(long, default_value = "data/experiments/phase4")]
        output: PathBuf,
        /// Boundary vehicles over the horizon, shaped by the diurnal profile. Not an OD matrix.
        #[arg(long, default_value_t = 6000.0)]
        daily_vehicles: f64,
        #[arg(long, default_value_t = 86400.0)]
        horizon_s: f64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Measure tick throughput, memory, scaling, and frame encoding on a compiled network.
    Benchmark {
        #[arg(long)]
        cache: Option<PathBuf>,
        #[arg(long, default_value = "thessaloniki")]
        preset: String,
        /// `tick` or `day`. The suite spawns this itself.
        #[arg(long)]
        worker: Option<String>,
        #[arg(long)]
        threads: Option<usize>,
        #[arg(long)]
        vehicles: Option<u64>,
        #[arg(long)]
        measure_ticks: Option<u32>,
        #[arg(long)]
        warmup_s: Option<f64>,
        #[arg(long)]
        daily_vehicles: Option<f64>,
        #[arg(long)]
        horizon_s: Option<f64>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long)]
        output: Option<PathBuf>,
        /// `all`, `loads`, `scale`, `day`, or `static`.
        #[arg(long, default_value = "all")]
        only: String,
    },
    /// Report link length and Courant number on a compiled cache.
    Audit {
        #[arg(long)]
        cache: Option<PathBuf>,
        /// Named city cache. `thessaloniki` loads `data/cache/thessaloniki.simnet`.
        #[arg(long, default_value = "thessaloniki")]
        preset: String,
    },
}

fn main() {
    if let Err(error) = dispatch() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn dispatch() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Ingest {
            preset,
            osm,
            bbox,
            cache_dir,
            refresh,
        } => {
            let source = if let Some(name) = preset {
                IngestSource::Preset(name)
            } else if let Some(path) = osm {
                IngestSource::OsmFile { path, bbox: None }
            } else if let Some(bbox) = bbox {
                IngestSource::BoundingBox(parse_bbox(&bbox)?)
            } else {
                IngestSource::Preset("thessaloniki".into())
            };
            let mut request =
                IngestRequest::new(source, cache_dir.unwrap_or_else(default_data_dir));
            request.refresh = refresh;
            let output = run(request)?;
            let stats = &output.network.stats;
            println!("cache {}", output.cache_path.display());
            if let Some(raw) = &output.raw_path {
                println!("raw {}", raw.display());
            }
            println!(
                "nodes {}  edges {}  turns {}  banned {}  signals {}  gateways {}  zones {}",
                stats.nodes,
                stats.edges,
                stats.turns,
                stats.banned_turns,
                stats.signals,
                stats.gateways,
                stats.zones
            );
            println!(
                "length {:.1} km  lane-km {:.1}  mean edge {:.1} m  contractions {}  diagram {}",
                stats.total_length_m / 1000.0,
                stats.total_lane_m / 1000.0,
                stats.mean_edge_length_m,
                stats.contractions,
                stats.diagram
            );
            let census = output.network.weak_components();
            let share = 100.0 * census.largest_length_m / stats.total_length_m.max(1.0);
            println!(
                "components {}  largest {:.1}% of length ({} nodes)  isolated {}",
                census.components, share, census.largest_nodes, census.isolated_nodes
            );
            for warning in &output.warnings {
                println!("warning: {warning}");
            }
        }
        Command::Experiment {
            headless,
            runs,
            cache,
            preset,
            output,
            daily_vehicles,
            horizon_s,
            seed,
        } => {
            if !headless {
                return Err("pass --headless to run the paired Monte Carlo batch".into());
            }
            let cache = cache.unwrap_or_else(|| cache_for_preset(&preset));
            experiment::run_headless(&cache, &output, runs, daily_vehicles, horizon_s, seed)?;
        }
        Command::Inspect { cache } => {
            let network = load_network(&cache)?;
            let stats = &network.stats;
            println!(
                "nodes {}  edges {}  length {:.1} km  signals {}  diagram {}",
                stats.nodes,
                stats.edges,
                stats.total_length_m / 1000.0,
                stats.signals,
                stats.diagram
            );
        }
        Command::Run {
            cache,
            preset,
            scenario,
            daily_vehicles,
            horizon_s,
            seed,
            output,
        } => {
            let cache = cache.unwrap_or_else(|| cache_for_preset(&preset));
            experiment::run_one(
                &cache,
                &scenario,
                output.as_deref(),
                daily_vehicles,
                horizon_s,
                seed,
            )?;
        }
        Command::Audit { cache, preset } => {
            let cache = cache.unwrap_or_else(|| cache_for_preset(&preset));
            audit::report(&cache)?;
        }
        Command::Benchmark {
            cache,
            preset,
            worker,
            threads,
            vehicles,
            measure_ticks,
            warmup_s,
            daily_vehicles,
            horizon_s,
            seed,
            output,
            only,
        } => {
            let cache = cache.unwrap_or_else(|| cache_for_preset(&preset));
            benchmark::dispatch(benchmark::BenchRequest {
                cache,
                worker,
                threads,
                vehicles,
                measure_ticks: measure_ticks.unwrap_or(90),
                warmup_s: warmup_s.unwrap_or(45.0),
                daily_vehicles: daily_vehicles.unwrap_or(100_000.0),
                horizon_s: horizon_s.unwrap_or(86_400.0),
                seed,
                output: output.unwrap_or_else(|| PathBuf::from("docs/perf-results.json")),
                only,
            })?;
        }
    }
    Ok(())
}

fn cache_for_preset(preset: &str) -> PathBuf {
    if preset == "thessaloniki" {
        if let Some(path) = sim_ingest::thessaloniki_cache(&[]) {
            return path;
        }
    }
    let name = format!("{preset}.simnet");
    let rooted = sim_ingest::default_data_dir().join("cache").join(&name);
    if rooted.exists() {
        return rooted;
    }
    let candidates = [
        PathBuf::from("data/cache").join(&name),
        PathBuf::from("../data/cache").join(&name),
    ];
    candidates
        .into_iter()
        .find(|path| Path::new(path).exists())
        .unwrap_or(rooted)
}

fn parse_bbox(text: &str) -> Result<BBox, String> {
    let parts: Vec<f64> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f64>()
                .map_err(|error| error.to_string())
        })
        .collect::<Result<_, _>>()?;
    if parts.len() != 4 {
        return Err("bbox must be min_lat,min_lon,max_lat,max_lon".into());
    }
    BBox {
        min_lat: parts[0],
        min_lon: parts[1],
        max_lat: parts[2],
        max_lon: parts[3],
    }
    .validate()
    .map_err(|error| error.to_string())
}
