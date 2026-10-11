//! Discrete mesoscopic step for a compiled network.
//!
//! The graph, fundamental diagram, and signal plans stay in `sim-core`.
//! This crate owns the clock, the vehicle queues, and the route decisions.

mod engine;
mod route;

pub use engine::{Census, EngineError, Simulation, Trip, Vehicle};
pub use route::should_reroute;
