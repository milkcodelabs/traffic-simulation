//! Discrete mesoscopic step for a compiled network.
//!
//! The graph, fundamental diagram, and signal plans stay in `sim-core`.
//! This crate owns the clock, the vehicle queues, and the route decisions.

mod batch;
mod demand;
mod diff;
mod engine;
mod overlay;
mod route;

pub use batch::{
    baseline_day_scenario, night_flashing_scenario, run_scenario, run_signal_experiment, ArmOutput,
    ExperimentConfig, ExperimentOutput, RunTotals,
};
pub use demand::{
    boundary_demand, emit_gateway_trips, emit_seeded_gateway_trips, entry_gateways, exit_gateways,
    gateway_nodes, gateway_zone_scales, retain_routable, BoundaryDemand, DemandCursor,
    SeededCursor,
};
pub use diff::{
    compare_edge, distribution, Distribution, DIFF_DEGRADED, DIFF_IMPROVED, DIFF_NEUTRAL,
};
pub use engine::{
    BufferFootprint, Census, EdgeObservation, EngineError, SimSnapshot, Simulation, StepTimings,
    Trip, Vehicle,
};
pub use overlay::{closure_scale, is_ring_or_motorway, lane_closure_scales, text_is_ring};
pub use route::local_router_scratch_bytes;
pub use route::should_reroute;
