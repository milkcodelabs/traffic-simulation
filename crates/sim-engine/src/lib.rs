//! Discrete mesoscopic step for a compiled network.
//!
//! The graph, fundamental diagram, and signal plans stay in `sim-core`.
//! This crate owns the clock, the vehicle queues, and the route decisions.

mod demand;
mod engine;
mod route;

pub use demand::{emit_gateway_trips, gateway_nodes, DemandCursor};
pub use engine::{Census, EngineError, SimSnapshot, Simulation, Trip, Vehicle};
pub use route::should_reroute;
