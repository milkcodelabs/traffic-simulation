//! Wire types for the desktop shell. Simulation frames are a later blob; this crate
//! carries ingestion commands and the static viewport geometry.

mod geometry;
mod protocol;

pub use geometry::{encode_viewport, VIEWPORT_MAGIC};
pub use protocol::{BBoxDto, IngestReport, IngestRequestDto, PresetDto};
