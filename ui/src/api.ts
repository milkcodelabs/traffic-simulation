import { invoke, isTauri } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { IngestReport, IngestRequest, Preset } from "./protocol";

export function desktopShell(): boolean {
  return isTauri();
}

export async function listPresets(): Promise<Preset[]> {
  return invoke<Preset[]>("list_presets");
}

export async function ingestNetwork(request: IngestRequest): Promise<IngestReport> {
  return invoke<IngestReport>("ingest_network", { request });
}

export async function loadGeometry(cachePath: string): Promise<ArrayBuffer> {
  const bytes = await invoke<ArrayBuffer>("network_geometry", { cachePath });
  return bytes;
}

export async function compiledCache(): Promise<string | null> {
  return invoke<string | null>("compiled_cache");
}

export async function openSimulation(cachePath: string): Promise<void> {
  return invoke("open_simulation", { cachePath });
}

export type SimulationControl = {
  paused?: boolean;
  speed?: number;
  demandPerHour?: number;
  signal?: "clock" | "fixed" | "flashing";
  seekS?: number;
};

export async function controlSimulation(control: SimulationControl): Promise<void> {
  return invoke("simulation_control", { control });
}

export async function applyScenario(control: { speedScale?: number; demandScale?: number; lanesClosed?: number }): Promise<void> {
  return invoke("simulation_scenario", { control });
}

export async function captureBaseline(): Promise<void> {
  return invoke("simulation_capture_baseline");
}

export async function restartSimulation(): Promise<void> {
  return invoke("simulation_restart");
}

export async function readDiff(): Promise<ArrayBuffer> {
  const bytes = await invoke<ArrayBuffer>("simulation_diff");
  return bytes;
}

export async function readFrame(since: number): Promise<ArrayBuffer> {
  return invoke<ArrayBuffer>("simulation_frame", { since });
}

export async function simulationStatus(): Promise<{ running: boolean; error: string | null }> {
  return invoke("simulation_status");
}

export async function chooseOsmFile(): Promise<string | null> {
  const selected = await open({
    multiple: false,
    filters: [{ name: "OpenStreetMap", extensions: ["osm", "xml", "pbf"] }],
  });
  return typeof selected === "string" ? selected : null;
}
