use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sim_core::{store_network, BBox, DiagramFamily, FlatElevation, Network};

use crate::compile::{compile_document, CompileConfig};
use crate::error::IngestError;
use crate::model::OsmDocument;
use crate::pbf::parse_pbf_file;
use crate::preset::preset_by_id;
use crate::xml::parse_xml_file;

const OVERPASS_ENDPOINTS: &[&str] = &[
    "https://lz4.overpass-api.de/api/interpreter",
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
    "https://overpass.openstreetmap.fr/api/interpreter",
];

#[derive(Clone, Debug)]
pub enum IngestSource {
    Preset(String),
    BoundingBox(BBox),
    OsmFile { path: PathBuf, bbox: Option<BBox> },
}

#[derive(Clone, Debug)]
pub struct IngestRequest {
    pub source: IngestSource,
    pub data_dir: PathBuf,
    pub refresh: bool,
    pub diagram: DiagramFamily,
}

impl IngestRequest {
    pub fn new(source: IngestSource, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            source,
            data_dir: data_dir.into(),
            refresh: false,
            diagram: DiagramFamily::Triangular,
        }
    }
}

pub struct IngestOutput {
    pub cache_path: PathBuf,
    pub raw_path: Option<PathBuf>,
    pub network: Network,
    pub warnings: Vec<String>,
}

pub fn default_data_dir() -> PathBuf {
    for start in search_starts() {
        if let Some(root) = find_workspace(&start) {
            return root.join("data");
        }
    }
    for start in search_starts() {
        if let Some(dir) = bundled_data(&start) {
            return dir;
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("data")
}

/// Workspace root that contains `crates/sim-core`.
///
/// A packaged binary is often started with a working directory that is not the
/// repo, so the search also walks up from the executable. An installed copy
/// with no repo nearby uses a `data/` directory beside the executable.
pub fn workspace_root() -> PathBuf {
    for start in search_starts() {
        if let Some(found) = find_workspace(&start) {
            return found;
        }
    }
    search_starts()
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("."))
}

fn search_starts() -> Vec<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            starts.push(dir.to_path_buf());
        }
    }
    starts
}

fn find_workspace(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    for _ in 0..8 {
        if dir.join("crates").join("sim-core").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn bundled_data(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    for _ in 0..4 {
        for candidate in [dir.join("data"), dir.join("resources").join("data")] {
            if candidate.join("cache").is_dir() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Compiled Thessaloniki graph, without contacting Overpass.
///
/// A checkout uses `data/cache/thessaloniki.simnet`. A packaged desktop looks at
/// the extra paths the shell passes in (the Tauri resource directory) and at
/// `resources/data/cache` beside the executable.
pub fn thessaloniki_cache(bundled: &[PathBuf]) -> Option<PathBuf> {
    const NAME: &str = "thessaloniki.simnet";
    if let Some(root) = search_starts()
        .into_iter()
        .find_map(|start| find_workspace(&start))
    {
        let path = root.join("data").join("cache").join(NAME);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates = bundled.to_vec();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("resources").join("data").join("cache").join(NAME));
            candidates.push(dir.join("data").join("cache").join(NAME));
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

pub fn run(request: IngestRequest) -> Result<IngestOutput, IngestError> {
    let (document, bbox, raw_path, preset_id, cache_name) = resolve(&request)?;
    let mut config = CompileConfig::new(bbox);
    config.diagram = request.diagram;
    config.preset_id = preset_id;
    let output = compile_document(&document, &config, &FlatElevation { elevation_m: 0.0 })?;
    let cache_path = request.data_dir.join("cache").join(cache_name);
    store_network(&cache_path, &output.network)?;
    Ok(IngestOutput {
        cache_path,
        raw_path,
        network: output.network,
        warnings: output.warnings.messages(),
    })
}

type ResolvedSource = (OsmDocument, BBox, Option<PathBuf>, Option<String>, String);

fn resolve(request: &IngestRequest) -> Result<ResolvedSource, IngestError> {
    match &request.source {
        IngestSource::Preset(name) => {
            let preset =
                preset_by_id(name).ok_or_else(|| IngestError::UnknownPreset(name.clone()))?;
            let raw_path = request
                .data_dir
                .join("raw")
                .join(format!("{}.osm.xml", preset.id));
            ensure_overpass(preset.bbox, &raw_path, request.refresh)?;
            let document = parse_xml_file(&raw_path)?;
            Ok((
                document,
                preset.bbox,
                Some(raw_path),
                Some(preset.id.to_string()),
                format!("{}.simnet", preset.id),
            ))
        }
        IngestSource::BoundingBox(bbox) => {
            let bbox = bbox.validate()?;
            let name = bbox_name(bbox);
            let raw_path = request.data_dir.join("raw").join(format!("{name}.osm.xml"));
            ensure_overpass(bbox, &raw_path, request.refresh)?;
            let document = parse_xml_file(&raw_path)?;
            Ok((
                document,
                bbox,
                Some(raw_path),
                None,
                format!("{name}.simnet"),
            ))
        }
        IngestSource::OsmFile { path, bbox } => {
            let document = parse_path(path)?;
            let bbox = match bbox {
                Some(bbox) => bbox.validate()?,
                None => padded_extent(&document)?,
            };
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("network")
                .replace('.', "_");
            Ok((
                document,
                bbox,
                Some(path.clone()),
                None,
                format!("{stem}.simnet"),
            ))
        }
    }
}

fn parse_path(path: &Path) -> Result<OsmDocument, IngestError> {
    let name = path.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".pbf") {
        parse_pbf_file(path)
    } else {
        parse_xml_file(path)
    }
}

fn padded_extent(document: &OsmDocument) -> Result<BBox, IngestError> {
    let mut min_lat = f64::INFINITY;
    let mut min_lon = f64::INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    for node in document.nodes.values() {
        min_lat = min_lat.min(node.lat);
        max_lat = max_lat.max(node.lat);
        min_lon = min_lon.min(node.lon);
        max_lon = max_lon.max(node.lon);
    }
    if !min_lat.is_finite() {
        return Err(IngestError::Empty);
    }
    BBox {
        min_lat: min_lat - 0.01,
        min_lon: min_lon - 0.01,
        max_lat: max_lat + 0.01,
        max_lon: max_lon + 0.01,
    }
    .validate()
    .map_err(IngestError::from)
}

fn bbox_name(bbox: BBox) -> String {
    format!(
        "bbox_{:.4}_{:.4}_{:.4}_{:.4}",
        bbox.min_lat, bbox.min_lon, bbox.max_lat, bbox.max_lon
    )
    .replace('-', "m")
    .replace('.', "p")
}

fn ensure_overpass(bbox: BBox, dest: &Path, refresh: bool) -> Result<(), IngestError> {
    if !refresh
        && dest.is_file()
        && fs::metadata(dest)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false)
    {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|source| IngestError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let bytes = download_overpass(bbox)?;
    let temp = dest.with_extension("download");
    {
        let mut file = fs::File::create(&temp).map_err(|source| IngestError::Io {
            path: temp.display().to_string(),
            source,
        })?;
        file.write_all(&bytes).map_err(|source| IngestError::Io {
            path: temp.display().to_string(),
            source,
        })?;
    }
    fs::rename(&temp, dest).map_err(|source| IngestError::Io {
        path: dest.display().to_string(),
        source,
    })?;
    Ok(())
}

fn download_overpass(bbox: BBox) -> Result<Vec<u8>, IngestError> {
    let query = overpass_query(bbox);
    let client = reqwest::blocking::Client::builder()
        .user_agent("traffic-simulation/0.1 (research; urban traffic model)")
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|error| IngestError::Download(error.to_string()))?;
    let mut errors = Vec::new();
    for endpoint in OVERPASS_ENDPOINTS {
        match client
            .post(*endpoint)
            .form(&[("data", query.as_str())])
            .send()
        {
            Ok(response) if response.status().is_success() => match response.bytes() {
                Ok(bytes) => {
                    let body = String::from_utf8_lossy(&bytes);
                    if body.contains("<osm") && body.contains("</osm>") {
                        return Ok(bytes.to_vec());
                    }
                    errors.push(format!(
                        "{endpoint} returned a body that is not a complete OSM document"
                    ));
                }
                Err(error) => errors.push(format!("{endpoint}: {error}")),
            },
            Ok(response) => errors.push(format!("{endpoint} returned HTTP {}", response.status())),
            Err(error) => errors.push(format!("{endpoint}: {error}")),
        }
    }
    Err(IngestError::Download(errors.join("; ")))
}

fn overpass_query(bbox: BBox) -> String {
    const CLASSES: &[&str] = &[
        "motorway",
        "motorway_link",
        "trunk",
        "trunk_link",
        "primary",
        "primary_link",
        "secondary",
        "secondary_link",
        "tertiary",
        "tertiary_link",
        "unclassified",
        "residential",
        "living_street",
        "service",
    ];
    let mut query = String::from("[out:xml][timeout:240];\n(\n");
    for class in CLASSES {
        query.push_str(&format!(
            "  way[\"highway\"=\"{class}\"]({south},{west},{north},{east});\n",
            south = bbox.min_lat,
            west = bbox.min_lon,
            north = bbox.max_lat,
            east = bbox.max_lon,
        ));
    }
    query.push_str(&format!(
        "  relation[\"type\"=\"restriction\"]({south},{west},{north},{east});\n);\n(._;>;);\nout body;\n",
        south = bbox.min_lat,
        west = bbox.min_lon,
        north = bbox.max_lat,
        east = bbox.max_lon,
    ));
    query
}

#[cfg(test)]
mod tests {
    use super::{bundled_data, find_workspace};
    use std::path::PathBuf;

    #[test]
    fn the_workspace_is_found_from_the_ingest_crate_directory() {
        let nested = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = find_workspace(&nested).expect("workspace root");
        assert!(root.join("crates").join("sim-core").is_dir());
        assert!(root.join("Cargo.toml").is_file());
    }

    #[test]
    fn a_cache_directory_beside_the_executable_is_a_data_root() {
        let root = std::env::temp_dir().join(format!("traffic-data-{}", std::process::id()));
        let cache = root.join("data").join("cache");
        std::fs::create_dir_all(&cache).expect("cache dir");
        let found = bundled_data(&root).expect("bundled data");
        assert_eq!(found, root.join("data"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_bundled_thessaloniki_cache_is_a_simnet_file() {
        use std::io::Read;
        let path = super::thessaloniki_cache(&[]).expect("data/cache/thessaloniki.simnet");
        let mut magic = [0_u8; 4];
        std::fs::File::open(&path)
            .and_then(|mut file| file.read_exact(&mut magic))
            .expect("read cache magic");
        assert_eq!(&magic, b"TSIM");
    }
}
