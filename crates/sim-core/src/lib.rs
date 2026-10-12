//! Static road graph and the physical quantities the link-transmission step will advance.
//!
//! OpenStreetMap parsing lives in `sim-ingest`. This crate owns the compiled network,
//! the fundamental diagram, grades, turns, signal plans, the diurnal shape, and the
//! vehicle-conservation state.

pub mod cache;
pub mod demand;
pub mod fd;
pub mod geo;
pub mod grade;
pub mod graph;
pub mod id;
pub mod realize;
pub mod road;
pub mod scenario;
pub mod signal;
pub mod state;
pub mod time;
pub mod turns;
pub mod utm;

pub use cache::{load_network, store_network, CACHE_MAGIC, FORMAT_VERSION};
pub use demand::{DiurnalProfile, DEFAULT_ROUTE_INERTIA};
pub use fd::{
    equilibrium_flow, greenshields_flow, smulders_flow, triangular_capacity, triangular_flow, DiagramFamily,
};
pub use geo::{polyline_length_m, BBox, LonLat, Xy};
pub use grade::{
    saturation_capacity_factor, speed_grade_factor, ElevationSampler, FlatElevation, GradeSample, GridElevation,
    DEFAULT_HEAVY_VEHICLE_SHARE,
};
pub use graph::{
    classify_node, Adjacency, AssembleInput, ComponentCensus, Edge, EdgeFlags, Network, NetworkStats, Node,
    NodeClassHints, NodeKind, Turn, Zone, ZoneKind,
};
pub use id::{EdgeId, NodeId, SignalId, TurnId, ZoneId};
pub use realize::{realize_edge, EdgeDraft, RealizeError};
pub use road::{
    default_lanes, default_speed_mps, jam_density_per_lane, parse_highway, wave_speed_mps, RoadClass,
    PRIMARY_RANK,
};
pub use scenario::{scale_factors, scaled_capacity, scaled_speed, EdgeScale, Scenario, SignalSchedule, ZoneScale};
pub use signal::{mode_label, plan_signal, Approach, Phase, SignalController, SignalMode, TAU_YIELD_S};
pub use state::{storage_inflow_ceiling, LinkTransmissionState, CONSERVATION_EPSILON};
pub use time::TimeWindow;
pub use turns::{classify_movement, Arrival, Movement};
pub use utm::{boundary_distance_m, UtmProjection};

/// Errors from diagram construction, graph validation, and the cache envelope.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("bounding box is empty or out of range")]
    InvalidBBox,
    #[error("diagram parameters are not physical: {0}")]
    Diagram(String),
    #[error("{0}")]
    Realize(#[from] RealizeError),
    #[error("graph invariant violated: {0}")]
    Invariant(String),
    #[error("cache magic is not TSIM")]
    CacheMagic,
    #[error("cache format {found} is not supported (this build reads {expected})")]
    CacheVersion { found: u32, expected: u32 },
    #[error("cache payload could not be decoded: {0}")]
    CachePayload(String),
    #[error("cache file {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}
