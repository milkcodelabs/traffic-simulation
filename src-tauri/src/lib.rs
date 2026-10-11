use std::path::PathBuf;
use std::sync::Mutex;

use sim_core::load_network;
use sim_ingest::{default_data_dir, presets, run as compile_network, IngestRequest, IngestSource};
use sim_ipc::{encode_viewport, IngestReport, IngestRequestDto, PresetDto};
use tauri::ipc::Response;
use tauri::State;

struct AppState {
    last_cache: Mutex<Option<PathBuf>>,
}

#[tauri::command]
fn list_presets() -> Vec<PresetDto> {
    presets()
        .iter()
        .map(|preset| PresetDto {
            id: preset.id.to_string(),
            label: preset.label.to_string(),
            bbox: preset.bbox.into(),
        })
        .collect()
}

#[tauri::command]
fn ingest_network(state: State<AppState>, request: IngestRequestDto) -> Result<IngestReport, String> {
    let job = match request {
        IngestRequestDto::Preset { name, refresh } => {
            let mut job = IngestRequest::new(IngestSource::Preset(name), default_data_dir());
            job.refresh = refresh;
            job
        }
        IngestRequestDto::BoundingBox { bbox, refresh } => {
            let mut job = IngestRequest::new(IngestSource::BoundingBox(bbox.into()), default_data_dir());
            job.refresh = refresh;
            job
        }
        IngestRequestDto::OsmFile { path } => IngestRequest::new(
            IngestSource::OsmFile {
                path: PathBuf::from(path),
                bbox: None,
            },
            default_data_dir(),
        ),
    };
    let output = compile_network(job).map_err(|error| error.to_string())?;
    if let Ok(mut slot) = state.last_cache.lock() {
        *slot = Some(output.cache_path.clone());
    }
    Ok(IngestReport {
        cache_path: output.cache_path.display().to_string(),
        raw_path: output.raw_path.map(|path| path.display().to_string()),
        stats: output.network.stats,
        warnings: output.warnings,
    })
}

#[tauri::command]
fn network_summary(cache_path: String) -> Result<IngestReport, String> {
    let network = load_network(std::path::Path::new(&cache_path)).map_err(|error| error.to_string())?;
    Ok(IngestReport {
        cache_path,
        raw_path: None,
        stats: network.stats,
        warnings: Vec::new(),
    })
}

#[tauri::command]
fn network_geometry(cache_path: String) -> Result<Response, String> {
    let network = load_network(std::path::Path::new(&cache_path)).map_err(|error| error.to_string())?;
    Ok(Response::new(encode_viewport(&network)))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            last_cache: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            list_presets,
            ingest_network,
            network_summary,
            network_geometry
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the traffic simulation shell");
}
