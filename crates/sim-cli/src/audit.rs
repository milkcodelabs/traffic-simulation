//! Geometric discretization report for a compiled cache.
//!
//! This does not re-run the invariant suite. That suite is
//! `cargo test -p sim-engine --test scientific_audit`.

use std::path::Path;

use sim_core::load_network;

pub fn report(cache: &Path) -> Result<(), Box<dyn std::error::Error>> {
    const DT_S: f64 = 1.0;
    let network = load_network(cache)?;
    let stats = &network.stats;
    let mut min_length_m = f64::INFINITY;
    let mut max_courant = 0.0_f64;
    let mut over_cfl = 0_usize;
    let mut nonpositive = 0_usize;
    for edge in &network.edges {
        if edge.length_m <= 0.0 {
            nonpositive += 1;
            continue;
        }
        min_length_m = min_length_m.min(edge.length_m);
        let courant = edge.free_flow_speed_mps * DT_S / edge.length_m;
        max_courant = max_courant.max(courant);
        if courant > 1.0 {
            over_cfl += 1;
        }
    }
    if !min_length_m.is_finite() {
        min_length_m = 0.0;
    }
    let edges = network.edges.len();
    let share = if edges == 0 {
        0.0
    } else {
        100.0 * over_cfl as f64 / edges as f64
    };
    println!("cache {}", cache.display());
    println!(
        "nodes {}  edges {}  signals {}  length {:.1} km  diagram {}",
        stats.nodes,
        stats.edges,
        stats.signals,
        stats.total_length_m / 1000.0,
        stats.diagram
    );
    println!(
        "dt_s {DT_S:.1}  min_length_m {min_length_m:.2}  max_courant {max_courant:.3}  links_over_cfl {over_cfl} ({share:.1}%)  nonpositive_length {nonpositive}"
    );
    println!("invariant suite: cargo test -p sim-engine --test scientific_audit");
    Ok(())
}
