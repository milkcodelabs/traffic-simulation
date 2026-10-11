use serde::{Deserialize, Serialize};
use sim_core::{BBox, NetworkStats};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetDto {
    pub id: String,
    pub label: String,
    pub bbox: BBoxDto,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BBoxDto {
    pub min_lat: f64,
    pub min_lon: f64,
    pub max_lat: f64,
    pub max_lon: f64,
}

impl From<BBox> for BBoxDto {
    fn from(bbox: BBox) -> Self {
        Self {
            min_lat: bbox.min_lat,
            min_lon: bbox.min_lon,
            max_lat: bbox.max_lat,
            max_lon: bbox.max_lon,
        }
    }
}

impl From<BBoxDto> for BBox {
    fn from(bbox: BBoxDto) -> Self {
        Self {
            min_lat: bbox.min_lat,
            min_lon: bbox.min_lon,
            max_lat: bbox.max_lat,
            max_lon: bbox.max_lon,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IngestRequestDto {
    #[serde(rename = "preset", rename_all = "camelCase")]
    Preset { name: String, refresh: bool },
    #[serde(rename = "bbox", rename_all = "camelCase")]
    BoundingBox { bbox: BBoxDto, refresh: bool },
    #[serde(rename = "file", rename_all = "camelCase")]
    OsmFile { path: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    pub cache_path: String,
    pub raw_path: Option<String>,
    pub stats: NetworkStats,
    pub warnings: Vec<String>,
}
