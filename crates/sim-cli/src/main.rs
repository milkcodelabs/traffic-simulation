use std::path::PathBuf;

use clap::{Parser, Subcommand};
use sim_core::{load_network, BBox};
use sim_ingest::{default_data_dir, run, IngestRequest, IngestSource};

#[derive(Parser)]
#[command(name = "sim-cli", about = "Compile an OpenStreetMap extract into a traffic network cache.")]
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
    Summary {
        #[arg(long)]
        cache: PathBuf,
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
            let mut request = IngestRequest::new(source, cache_dir.unwrap_or_else(default_data_dir));
            request.refresh = refresh;
            let output = run(request)?;
            let stats = &output.network.stats;
            println!("cache {}", output.cache_path.display());
            if let Some(raw) = &output.raw_path {
                println!("raw {}", raw.display());
            }
            println!(
                "nodes {}  edges {}  turns {}  banned {}  signals {}  gateways {}  zones {}",
                stats.nodes, stats.edges, stats.turns, stats.banned_turns, stats.signals, stats.gateways, stats.zones
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
        Command::Summary { cache } => {
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
    }
    Ok(())
}

fn parse_bbox(text: &str) -> Result<BBox, String> {
    let parts: Vec<f64> = text
        .split(',')
        .map(|part| part.trim().parse::<f64>().map_err(|error| error.to_string()))
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
