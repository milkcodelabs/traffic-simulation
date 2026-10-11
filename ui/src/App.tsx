import { useEffect, useMemo, useState } from "react";
import { chooseOsmFile, desktopShell, ingestNetwork, listPresets, loadGeometry } from "./api";
import { NetworkMap } from "./NetworkMap";
import { decodeViewport, type BBox, type IngestReport, type Preset } from "./protocol";

type SourceMode = "preset" | "bbox" | "file";

const EMPTY_BOX: BBox = { minLat: 40.57, minLon: 22.885, maxLat: 40.675, maxLon: 23.015 };

export function App() {
  const desktop = desktopShell();
  const [presets, setPresets] = useState<Preset[]>([]);
  const [mode, setMode] = useState<SourceMode>("preset");
  const [presetId, setPresetId] = useState("thessaloniki");
  const [bbox, setBbox] = useState<BBox>(EMPTY_BOX);
  const [filePath, setFilePath] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [report, setReport] = useState<IngestReport | null>(null);
  const [network, setNetwork] = useState<GeoJSON.FeatureCollection | null>(null);

  useEffect(() => {
    if (!desktop) {
      return;
    }
    listPresets()
      .then((items) => {
        setPresets(items);
        const thessaloniki = items.find((item) => item.id === "thessaloniki");
        if (thessaloniki) {
          setPresetId(thessaloniki.id);
          setBbox(thessaloniki.bbox);
        }
      })
      .catch((reason: unknown) => setError(message(reason)));
  }, [desktop]);

  const center = useMemo<[number, number]>(() => {
    const box = report?.stats.bbox ?? bbox;
    return [(box.minLon + box.maxLon) / 2, (box.minLat + box.maxLat) / 2];
  }, [bbox, report]);

  async function compile() {
    setBusy(true);
    setError(null);
    try {
      const request =
        mode === "preset"
          ? { kind: "preset" as const, name: presetId, refresh }
          : mode === "bbox"
            ? { kind: "bbox" as const, bbox, refresh }
            : { kind: "file" as const, path: filePath ?? "" };
      if (request.kind === "file" && request.path.length === 0) {
        throw new Error("Choose an .osm, .xml, or .pbf file first.");
      }
      const next = await ingestNetwork(request);
      const bytes = await loadGeometry(next.cachePath);
      setReport(next);
      setNetwork(decodeViewport(bytes));
    } catch (reason) {
      setError(message(reason));
    } finally {
      setBusy(false);
    }
  }

  async function browse() {
    setError(null);
    try {
      const path = await chooseOsmFile();
      if (path) {
        setFilePath(path);
        setMode("file");
      }
    } catch (reason) {
      setError(message(reason));
    }
  }

  return (
    <main className="relative h-full overflow-hidden bg-[#10140f] text-[#e6e1d6]">
      <NetworkMap center={center} data={network} />
      <aside className="absolute top-4 left-4 z-10 flex max-h-[calc(100%-2rem)] w-[22.5rem] flex-col gap-4 overflow-auto rounded-md border border-[#2a3128] bg-[#161c14]/95 p-4 shadow-2xl backdrop-blur">
        <header>
          <p className="text-[0.68rem] tracking-[0.18em] text-[#a89878] uppercase">Urban traffic model</p>
          <h1 className="mt-1 text-xl font-semibold text-[#f3efe4]">Network compiler</h1>
          <p className="mt-1 text-sm leading-5 text-[#b7b1a4]">
            OpenStreetMap roads become a directed graph with storage, speeds, and signal plans. Thessaloniki is the calibration city.
          </p>
        </header>

        {!desktop && (
          <p className="rounded border border-[#6a5330] bg-[#2a2116] px-3 py-2 text-sm text-[#f0d7a8]">
            This page is the desktop shell. Ingestion runs inside the Tauri window, started with <span className="font-mono">npm run tauri dev</span>.
          </p>
        )}

        <fieldset className="space-y-2" disabled={busy}>
          <legend className="text-xs tracking-wide text-[#a89878] uppercase">Source</legend>
          <div className="grid grid-cols-3 gap-1">
            {(["preset", "bbox", "file"] as const).map((item) => (
              <button
                key={item}
                type="button"
                onClick={() => setMode(item)}
                className={`rounded px-2 py-1.5 text-sm ${mode === item ? "bg-[#d7c4a1] text-[#1b160f]" : "bg-[#232920] text-[#d9d3c6]"}`}
              >
                {item === "bbox" ? "Box" : item[0].toUpperCase() + item.slice(1)}
              </button>
            ))}
          </div>

          {mode === "preset" && (
            <label className="block text-sm">
              City
              <select
                value={presetId}
                onChange={(event) => {
                  const id = event.target.value;
                  setPresetId(id);
                  const preset = presets.find((item) => item.id === id);
                  if (preset) {
                    setBbox(preset.bbox);
                  }
                }}
                className="mt-1 w-full rounded border border-[#343d31] bg-[#10140f] px-2 py-1.5"
              >
                {(presets.length > 0 ? presets : [{ id: "thessaloniki", label: "Thessaloniki", bbox: EMPTY_BOX }]).map((preset) => (
                  <option key={preset.id} value={preset.id}>
                    {preset.label}
                  </option>
                ))}
              </select>
            </label>
          )}

          {mode !== "file" && <BBoxFields bbox={bbox} editable={mode === "bbox"} onChange={setBbox} />}

          {mode === "file" && (
            <div className="space-y-2">
              <button type="button" onClick={() => void browse()} disabled={!desktop} className="rounded bg-[#232920] px-3 py-1.5 text-sm disabled:opacity-50">
                Choose OSM file
              </button>
              <p className="font-mono text-xs break-all text-[#b7b1a4]">{filePath ?? "No file selected"}</p>
            </div>
          )}

          {mode !== "file" && (
            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={refresh} onChange={(event) => setRefresh(event.target.checked)} />
              Download again
            </label>
          )}

          <button
            type="button"
            onClick={() => void compile()}
            disabled={!desktop || busy}
            className="w-full rounded bg-[#d7c4a1] px-3 py-2 text-sm font-semibold text-[#1b160f] disabled:opacity-50"
          >
            {busy ? "Compiling network…" : "Compile network"}
          </button>
        </fieldset>

        {error && <p className="rounded border border-[#6d3030] bg-[#2a1616] px-3 py-2 text-sm text-[#f0c2c2]">{error}</p>}

        {report && <Stats report={report} />}
      </aside>
    </main>
  );
}

function BBoxFields({ bbox, editable, onChange }: { bbox: BBox; editable: boolean; onChange: (bbox: BBox) => void }) {
  const fields: Array<[keyof BBox, string]> = [
    ["minLat", "South"],
    ["minLon", "West"],
    ["maxLat", "North"],
    ["maxLon", "East"],
  ];
  return (
    <div className="grid grid-cols-2 gap-2">
      {fields.map(([key, label]) => (
        <label key={key} className="text-xs text-[#b7b1a4]">
          {label}
          <input
            value={bbox[key]}
            disabled={!editable}
            onChange={(event) => onChange({ ...bbox, [key]: Number(event.target.value) })}
            className="mt-1 w-full rounded border border-[#343d31] bg-[#10140f] px-2 py-1 font-mono text-sm text-[#f3efe4] disabled:opacity-70"
          />
        </label>
      ))}
    </div>
  );
}

function Stats({ report }: { report: IngestReport }) {
  const stats = report.stats;
  const rows: Array<[string, string]> = [
    ["Nodes", format(stats.nodes)],
    ["Edges", format(stats.edges)],
    ["Length", `${(stats.totalLengthM / 1000).toFixed(1)} km`],
    ["Lane-km", (stats.totalLaneM / 1000).toFixed(1)],
    ["Signals", format(stats.signals)],
    ["Gateways", format(stats.gateways)],
    ["Zones", format(stats.zones)],
    ["Banned turns", format(stats.bannedTurns)],
    ["Contractions", format(stats.contractions)],
    ["Diagram", stats.diagram],
  ];
  return (
    <section>
      <h2 className="text-xs tracking-wide text-[#a89878] uppercase">Compiled graph</h2>
      <dl className="mt-2 grid grid-cols-2 gap-x-3 gap-y-1 text-sm">
        {rows.map(([label, value]) => (
          <div key={label} className="contents">
            <dt className="text-[#b7b1a4]">{label}</dt>
            <dd className="text-right font-mono text-[#f3efe4]">{value}</dd>
          </div>
        ))}
      </dl>
      {report.warnings.length > 0 && (
        <ul className="mt-3 space-y-1 text-xs text-[#d7c4a1]">
          {report.warnings.map((warning) => (
            <li key={warning}>{warning}</li>
          ))}
        </ul>
      )}
      <p className="mt-3 font-mono text-[0.68rem] break-all text-[#8d8678]">{report.cachePath}</p>
    </section>
  );
}

function format(value: number) {
  return new Intl.NumberFormat("en-US").format(value);
}

function message(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason);
}
