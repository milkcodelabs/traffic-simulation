//! OpenStreetMap ingestion. The output is a `sim_core::Network`, never a live OSM graph.

mod clip;
mod compile;
mod error;
mod heuristics;
mod model;
mod pbf;
mod preset;
mod source;
mod xml;

pub use compile::{compile_document, CompileConfig, CompileOutput};
pub use error::IngestError;
pub use model::OsmDocument;
pub use preset::{preset_by_id, presets, CityPreset, THESSALONIKI_ID};
pub use source::{
    default_data_dir, run, thessaloniki_cache, workspace_root, IngestOutput, IngestRequest,
    IngestSource,
};
pub use xml::parse_xml_str;

pub fn load_cached(path: &std::path::Path) -> Result<sim_core::Network, IngestError> {
    Ok(sim_core::load_network(path)?)
}
