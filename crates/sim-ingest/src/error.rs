use sim_core::CoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("unknown city preset '{0}'")]
    UnknownPreset(String),
    #[error("OSM XML parse error: {0}")]
    Xml(String),
    #[error("OSM PBF parse error: {0}")]
    Pbf(String),
    #[error("download failed from every Overpass endpoint: {0}")]
    Download(String),
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("compiled network has no driveable edges")]
    Empty,
}
