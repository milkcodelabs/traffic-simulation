import { useEffect, useMemo, useRef, useState } from "react";
import {
  applyScenario,
  captureBaseline,
  chooseOsmFile,
  compiledCache,
  controlSimulation,
  desktopShell,
  ingestNetwork,
  listPresets,
  loadGeometry,
  openSimulation,
  readDiff,
  readFrame,
  restartSimulation,
  simulationStatus,
  type SimulationControl,
} from "./api";
import { Dashboard } from "./Dashboard";
import { decodeDiff, decodeFrame, type Bottleneck, type FrameHeader } from "./frame";
import { NetworkMap, type MapHandle } from "./NetworkMap";
import { decodeViewport, type BBox, type IngestReport, type Preset } from "./protocol";

let autoloadStarted = false;

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
  const [running, setRunning] = useState(false);
  const [header, setHeader] = useState<FrameHeader | null>(null);
  const [bottlenecks, setBottlenecks] = useState<Bottleneck[]>([]);
  const [viewFps, setViewFps] = useState(0);
  const [streamFps, setStreamFps] = useState(0);
  const [scrub, setScrub] = useState(0);
  const [speed, setSpeed] = useState(1);
  const [demand, setDemand] = useState(0);
  const [signal, setSignal] = useState<"clock" | "fixed" | "flashing">("clock");
  const [showQueue, setShowQueue] = useState(true);
  const [spark, setSpark] = useState<number[]>([]);
  const [speedScale, setSpeedScale] = useState(1);
  const [demandScale, setDemandScale] = useState(1);
  const [lanesClosed, setLanesClosed] = useState(0);
  const [diffOn, setDiffOn] = useState(false);
  const [diffCounts, setDiffCounts] = useState({
    improved: 0,
    degraded: 0,
    neutral: 0,
    baselineS: null as number | null,
    comparedS: 0,
    baselineVhtS: null as number | null,
    baselineDelayS: null as number | null,
    comparedVhtS: null as number | null,
    comparedDelayS: null as number | null,
  });
  const mapRef = useRef<MapHandle>(null);
  const dragging = useRef(false);
  const seen = useRef<FrameHeader | null>(null);

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

  useEffect(() => {
    if (!desktop || autoloadStarted) {
      return;
    }
    autoloadStarted = true;
    compiledCache()
      .then(async (path) => {
        if (!path) {
          return;
        }
        await showNetwork(path);
      })
      .catch((reason: unknown) => setError(message(reason)));
    // React remounts this effect once in development. A cleanup flag would drop the only load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [desktop]);

  useEffect(() => {
    if (!dragging.current && header) {
      setScrub(header.timeS % 86400);
    }
  }, [header]);

  useEffect(() => {
    if (!desktop || !running || !diffOn) {
      mapRef.current?.setDiff(null);
      return;
    }
    let stop = false;
    const pull = () => {
      if (stop) {
        return;
      }
      readDiff()
        .then((bytes) => {
          if (stop || bytes.byteLength < 36) {
            return;
          }
          const packet = decodeDiff(bytes);
          mapRef.current?.setDiff(packet.classes);
          setDiffCounts({
            improved: packet.improved,
            degraded: packet.degraded,
            neutral: packet.neutral,
            baselineS: packet.baselineS,
            comparedS: packet.comparedS,
            baselineVhtS: packet.baselineVhtS,
            baselineDelayS: packet.baselineDelayS,
            comparedVhtS: packet.comparedVhtS,
            comparedDelayS: packet.comparedDelayS,
          });
        })
        .catch((reason: unknown) => {
          if (!stop) {
            setError(message(reason));
          }
        });
    };
    pull();
    const timer = window.setInterval(pull, 400);
    return () => {
      stop = true;
      window.clearInterval(timer);
      mapRef.current?.setDiff(null);
    };
  }, [desktop, running, diffOn]);

  useEffect(() => {
    if (!desktop || !running) {
      return;
    }
    let stop = false;
    let since = 0;
    let views = 0;
    let streams = 0;
    let stamp = performance.now();
    let paintedAt = 0;
    let inflight = false;
    let statusTicks = 0;
    const tick = () => {
      if (stop) {
        return;
      }
      views += 1;
      const now = performance.now();
      if (now - stamp >= 1000) {
        setViewFps(views);
        setStreamFps(streams);
        views = 0;
        streams = 0;
        stamp = now;
      }
      if (!inflight) {
        inflight = true;
        readFrame(since)
          .then(async (bytes) => {
            if (stop) {
              return;
            }
            if (bytes.byteLength > 0) {
              const decoded = decodeFrame(bytes);
              since = decoded.header.sequence;
              streams += 1;
              if (!decoded.unchanged && decoded.speedRatio.length > 0) {
                mapRef.current?.setFields(decoded.speedRatio, decoded.densityRatio);
                setBottlenecks(decoded.bottlenecks);
              }
              const previous = seen.current;
              const flagsChanged =
                !previous ||
                previous.paused !== decoded.header.paused ||
                previous.seeking !== decoded.header.seeking;
              const clockChanged =
                !previous ||
                previous.timeS !== decoded.header.timeS ||
                previous.active !== decoded.header.active ||
                previous.meanSpeedMps !== decoded.header.meanSpeedMps;
              if (flagsChanged || (clockChanged && performance.now() - paintedAt >= 100)) {
                paintedAt = performance.now();
                seen.current = decoded.header;
                setHeader(decoded.header);
                if (
                  !previous ||
                  previous.meanSpeedMps !== decoded.header.meanSpeedMps ||
                  Math.floor(previous.timeS) !== Math.floor(decoded.header.timeS)
                ) {
                  setSpark((samples) => {
                    const next = samples.length >= 120 ? samples.slice(samples.length - 119) : samples.slice();
                    next.push(decoded.header.meanSpeedMps * 3.6);
                    return next;
                  });
                }
              }
            }
            statusTicks += 1;
            if (statusTicks % 90 === 0) {
              const status = await simulationStatus();
              if (!stop && status.error) {
                setError(status.error);
              }
            }
          })
          .catch((reason) => {
            if (!stop) {
              setError(message(reason));
            }
          })
          .finally(() => {
            inflight = false;
          });
      }
      requestAnimationFrame(tick);
    };
    const frameId = requestAnimationFrame(tick);
    return () => {
      stop = true;
      cancelAnimationFrame(frameId);
    };
  }, [desktop, running]);

  async function showNetwork(cachePath: string) {
    const bytes = await loadGeometry(cachePath);
    setNetwork(decodeViewport(bytes));
    await openSimulation(cachePath);
    setRunning(true);
    setHeader(null);
    setBottlenecks([]);
    setSpark([]);
    seen.current = null;
  }

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
      setReport(next);
      await showNetwork(next.cachePath);
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

  async function send(control: SimulationControl) {
    if (!desktop || !running) {
      return;
    }
    try {
      await controlSimulation(control);
    } catch (reason) {
      setError(message(reason));
    }
  }

  function edgeLabel(edge: number) {
    const feature = network?.features[edge];
    const name = feature?.properties && typeof feature.properties === "object" && "className" in feature.properties ? String(feature.properties.className) : "link";
    return name.replaceAll("_", " ");
  }

  return (
    <main className="relative h-full overflow-hidden bg-[#10140f] text-[#e6e1d6]">
      <NetworkMap ref={mapRef} center={center} data={network} showQueue={showQueue} />
      <Dashboard
        desktop={desktop}
        running={running}
        header={header}
        bottlenecks={bottlenecks}
        edgeLabel={edgeLabel}
        viewFps={viewFps}
        streamFps={streamFps}
        scrub={scrub}
        speed={speed}
        demand={demand}
        signal={signal}
        showQueue={showQueue}
        spark={spark}
        onScrub={(value, commit) => {
          dragging.current = !commit;
          setScrub(value);
          if (commit) {
            void send({ seekS: value });
          }
        }}
        onTogglePause={() => {
          const next = !(header?.paused ?? false);
          void send({ paused: next });
        }}
        onSpeed={(value) => {
          setSpeed(value);
          void send({ speed: value });
        }}
        onDemand={(value) => {
          if (!Number.isFinite(value)) {
            return;
          }
          const next = Math.max(0, Math.min(100000, value));
          setDemand(next);
          void send({ demandPerHour: next });
        }}
        onSignal={(value) => {
          setSignal(value);
          void send({ signal: value });
        }}
        onQueue={setShowQueue}
        onCamera={(preset) => mapRef.current?.camera(preset)}
        speedScale={speedScale}
        demandScale={demandScale}
        lanesClosed={lanesClosed}
        diffOn={diffOn}
        diffCounts={diffCounts}
        onSpeedScale={(value) => {
          setSpeedScale(value);
          void applyScenario({ speedScale: value }).catch((reason: unknown) => setError(message(reason)));
        }}
        onDemandScale={(value) => {
          setDemandScale(value);
          void applyScenario({ demandScale: value }).catch((reason: unknown) => setError(message(reason)));
        }}
        onLanes={(value) => {
          setLanesClosed(value);
          void applyScenario({ lanesClosed: value }).catch((reason: unknown) => setError(message(reason)));
        }}
        onCapture={() => {
          void captureBaseline().catch((reason: unknown) => setError(message(reason)));
        }}
        onRestart={() => {
          setScrub(0);
          void restartSimulation().catch((reason: unknown) => setError(message(reason)));
        }}
        onDiff={setDiffOn}
      />
      <aside className="absolute top-4 left-4 z-10 flex max-h-[calc(100%-6.5rem)] w-[22.5rem] flex-col gap-4 overflow-auto rounded-md border border-[#2a3128] bg-[#161c14]/95 p-4 shadow-2xl backdrop-blur">
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
