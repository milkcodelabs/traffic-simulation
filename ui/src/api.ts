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

export async function chooseOsmFile(): Promise<string | null> {
  const selected = await open({
    multiple: false,
    filters: [{ name: "OpenStreetMap", extensions: ["osm", "xml", "pbf"] }],
  });
  return typeof selected === "string" ? selected : null;
}
