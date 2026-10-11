//! Acceptance check for the calibration city. The raw Overpass document is cached under
//! `data/raw` and reused; the first run on a machine performs the download.

use std::path::PathBuf;

use sim_core::{load_network, store_network};
use sim_ingest::{run, IngestRequest, IngestSource};

#[test]
fn thessaloniki_preset_compiles_a_connected_graph() {
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let request = IngestRequest::new(IngestSource::Preset("thessaloniki".into()), &data_dir);
    let output = run(request).expect("Thessaloniki preset should download and compile");
    let network = &output.network;
    network.validate().expect("compiled graph should satisfy its invariants");

    assert_eq!(network.preset_id.as_deref(), Some("thessaloniki"));
    assert!(network.stats.nodes > 1_000, "nodes {}", network.stats.nodes);
    assert!(network.stats.edges > 1_000, "edges {}", network.stats.edges);
    assert!(network.stats.total_length_m > 100_000.0, "length {}", network.stats.total_length_m);
    assert!(network.stats.signals > 0, "a signalised city extract should keep signal nodes");
    assert!(network.stats.contractions > 0, "degree-2 vertices should contract");
    assert!(network.stats.banned_turns > 0, "OSM turn restrictions should ban movements");
    assert!(network.stats.gateways > 0, "the bounding box should cut gateways");

    let census = network.weak_components();
    let share = census.largest_length_m / network.stats.total_length_m;
    assert_eq!(census.isolated_nodes, 0);
    assert!(
        share > 0.8,
        "largest component holds {:.1}% of length across {} components",
        100.0 * share,
        census.components
    );

    for edge in &network.edges {
        if !edge.flags.dead_end_reversal {
            continue;
        }
        let paired = network.edges.iter().any(|other| {
            other.from == edge.to && other.to == edge.from && !other.flags.dead_end_reversal
        });
        assert!(paired, "dead-end reversal {} -> {} has no forward edge", edge.from.0, edge.to.0);
    }

    let copy = data_dir.join("cache").join("thessaloniki.roundtrip.simnet");
    store_network(&copy, network).unwrap();
    let first = std::fs::read(&output.cache_path).unwrap();
    let loaded = load_network(&output.cache_path).unwrap();
    assert_eq!(loaded.stats, network.stats);
    assert_eq!(loaded, *network);
    store_network(&copy, &loaded).unwrap();
    assert_eq!(first, std::fs::read(&copy).unwrap());
    let _ = std::fs::remove_file(&copy);

    println!(
        "thessaloniki nodes {} edges {} length {:.1} km signals {} gateways {} banned {} contractions {} components {} largest {:.1}%",
        network.stats.nodes,
        network.stats.edges,
        network.stats.total_length_m / 1000.0,
        network.stats.signals,
        network.stats.gateways,
        network.stats.banned_turns,
        network.stats.contractions,
        census.components,
        100.0 * share
    );
}
