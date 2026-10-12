use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sim_core::{load_network, SignalSchedule};
use sim_ingest::{default_data_dir, presets, run as compile_network, IngestRequest, IngestSource};
use sim_ipc::{IngestReport, IngestRequestDto, PresetDto};
use tauri::ipc::Response;
use tauri::State;

mod session;

use session::Session;

struct AppState {
    last_cache: Mutex<Option<PathBuf>>,
    session: Mutex<Option<Session>>,
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
    Ok(Response::new(encode_geometry(&network)))
}

fn encode_geometry(network: &sim_core::Network) -> Vec<u8> {
    sim_ipc::encode_viewport(network)
}

#[tauri::command]
fn compiled_cache() -> Option<String> {
    let path = default_data_dir().join("cache").join("thessaloniki.simnet");
    path.is_file().then(|| path.display().to_string())
}

#[tauri::command]
fn open_simulation(state: State<AppState>, cache_path: String) -> Result<(), String> {
    let network = load_network(std::path::Path::new(&cache_path)).map_err(|error| error.to_string())?;
    let session = Session::spawn(network);
    let mut slot = state.session.lock().map_err(|error| error.to_string())?;
    *slot = Some(session);
    if let Ok(mut cache) = state.last_cache.lock() {
        *cache = Some(PathBuf::from(cache_path));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimulationControl {
    paused: Option<bool>,
    speed: Option<f64>,
    demand_per_hour: Option<f64>,
    signal: Option<String>,
    seek_s: Option<f64>,
}

#[tauri::command]
fn simulation_control(state: State<AppState>, control: SimulationControl) -> Result<(), String> {
    let slot = state.session.lock().map_err(|error| error.to_string())?;
    let Some(session) = slot.as_ref() else {
        return Err("Compile a network before running the simulation.".into());
    };
    if let Some(paused) = control.paused {
        session.pause(paused)?;
    }
    if let Some(speed) = control.speed {
        session.set_speed(speed)?;
    }
    if let Some(demand) = control.demand_per_hour {
        session.set_demand(demand)?;
    }
    if let Some(signal) = control.signal {
        session.set_signal(parse_signal(&signal)?)?;
    }
    if let Some(seek_s) = control.seek_s {
        session.seek(seek_s)?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimulationStatus {
    running: bool,
    error: Option<String>,
}

#[tauri::command]
fn simulation_status(state: State<AppState>) -> Result<SimulationStatus, String> {
    let slot = state.session.lock().map_err(|error| error.to_string())?;
    let Some(session) = slot.as_ref() else {
        return Ok(SimulationStatus {
            running: false,
            error: None,
        });
    };
    let (running, error) = session.status();
    Ok(SimulationStatus { running, error })
}

#[tauri::command]
fn simulation_frame(state: State<AppState>, since: u64) -> Result<Response, String> {
    let slot = state.session.lock().map_err(|error| error.to_string())?;
    let Some(session) = slot.as_ref() else {
        return Ok(Response::new(Vec::new()));
    };
    Ok(Response::new(session.frame_since(since)))
}

fn parse_signal(value: &str) -> Result<SignalSchedule, String> {
    match value {
        "clock" => Ok(SignalSchedule::Unchanged),
        "fixed" => Ok(SignalSchedule::ForceFixed),
        "flashing" => Ok(SignalSchedule::ForceFlashingAmber),
        other => Err(format!("unknown signal schedule {other}")),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            last_cache: Mutex::new(None),
            session: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            list_presets,
            ingest_network,
            network_summary,
            network_geometry,
            compiled_cache,
            open_simulation,
            simulation_control,
            simulation_status,
            simulation_frame
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the traffic simulation shell");
}
