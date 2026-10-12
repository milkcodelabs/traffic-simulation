import type { Bottleneck, FrameHeader } from "./frame";
import { clockLabel } from "./frame";

const SPEEDS = [1, 2, 5, 10, 20, 50, 100] as const;

type SignalMode = "clock" | "fixed" | "flashing";

type Props = {
  desktop: boolean;
  running: boolean;
  header: FrameHeader | null;
  bottlenecks: Bottleneck[];
  edgeLabel: (edge: number) => string;
  viewFps: number;
  streamFps: number;
  scrub: number;
  speed: number;
  demand: number;
  signal: SignalMode;
  showQueue: boolean;
  spark: number[];
  onScrub: (value: number, commit: boolean) => void;
  onTogglePause: () => void;
  onSpeed: (value: number) => void;
  onDemand: (value: number) => void;
  onSignal: (value: SignalMode) => void;
  onQueue: (value: boolean) => void;
  onCamera: (preset: "fit" | "city" | "centre") => void;
};

export function Dashboard(props: Props) {
  const header = props.header;
  const time = props.scrub;
  const paused = header?.paused ?? true;
  const seeking = header?.seeking ?? false;
  return (
    <>
      <aside className="absolute top-16 right-4 z-10 flex max-h-[calc(100%-7.5rem)] w-80 flex-col gap-3 overflow-auto rounded-md border border-[#2a3128] bg-[#161c14]/95 p-4 shadow-2xl backdrop-blur">
        <header>
          <p className="text-[0.68rem] tracking-[0.18em] text-[#a89878] uppercase">Research telemetry</p>
          <h2 className="mt-1 text-lg font-semibold text-[#f3efe4]">Live network</h2>
        </header>
        <dl className="grid grid-cols-2 gap-x-3 gap-y-2 text-sm">
          <Metric label="Mean speed" value={header ? `${(header.meanSpeedMps * 3.6).toFixed(1)} km/h` : "—"} />
          <Metric label="VHT" value={header ? `${(header.vhtS / 3600).toFixed(2)} h` : "—"} />
          <Metric label="VKT" value={header ? `${(header.vktM / 1000).toFixed(1)} km` : "—"} />
          <Metric label="Delay" value={header ? `${(header.delayS / 3600).toFixed(2)} h` : "—"} />
          <Metric label="Active" value={header ? format(header.active) : "—"} />
          <Metric label="Completed" value={header ? format(header.completed) : "—"} />
        </dl>
        <Sparkline samples={props.spark} />
        <section>
          <h3 className="text-xs tracking-wide text-[#a89878] uppercase">Top bottlenecks</h3>
          {props.bottlenecks.length === 0 ? (
            <p className="mt-2 text-sm text-[#b7b1a4]">No stopped vehicles. A free-flow network stays green.</p>
          ) : (
            <ol className="mt-2 space-y-1.5 text-sm">
              {props.bottlenecks.map((item) => (
                <li key={item.edge} className="flex items-baseline justify-between gap-2">
                  <span className="text-[#d9d3c6]">
                    <span className="font-mono text-[#f3efe4]">{item.edge}</span> {props.edgeLabel(item.edge)}
                  </span>
                  <span className="shrink-0 font-mono text-[#e2a23a]">
                    {item.queueVeh.toFixed(0)} veh · {(item.speedRatio * 100).toFixed(0)}%
                  </span>
                </li>
              ))}
            </ol>
          )}
        </section>
        <label className="block text-sm text-[#d9d3c6]">
          Boundary flow
          <span className="mt-1 flex items-center gap-2">
            <input
              type="number"
              min={0}
              max={100000}
              step={100}
              value={props.demand}
              disabled={!props.desktop || !props.running}
              onChange={(event) => props.onDemand(Number(event.target.value))}
              className="w-full rounded border border-[#343d31] bg-[#10140f] px-2 py-1.5 font-mono text-[#f3efe4] disabled:opacity-50"
            />
            <span className="shrink-0 text-xs text-[#b7b1a4]">veh/h</span>
          </span>
          <span className="mt-1 block text-xs leading-4 text-[#8d8678]">Departures at compiled gateway nodes. Zero leaves the roads empty.</span>
        </label>
        <fieldset className="space-y-2" disabled={!props.desktop || !props.running}>
          <legend className="text-xs tracking-wide text-[#a89878] uppercase">Signals</legend>
          <div className="grid grid-cols-3 gap-1">
            {(
              [
                ["clock", "Clock"],
                ["fixed", "Fixed"],
                ["flashing", "Flashing"],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                onClick={() => props.onSignal(id)}
                className={`rounded px-2 py-1.5 text-sm ${props.signal === id ? "bg-[#d7c4a1] text-[#1b160f]" : "bg-[#232920] text-[#d9d3c6]"}`}
              >
                {label}
              </button>
            ))}
          </div>
          <p className="text-xs leading-4 text-[#8d8678]">Clock uses flashing amber from 00:00 to 06:00 and the fixed cycle after that.</p>
        </fieldset>
        <label className="flex items-center gap-2 text-sm text-[#d9d3c6]">
          <input type="checkbox" checked={props.showQueue} onChange={(event) => props.onQueue(event.target.checked)} />
          Queue density
        </label>
        <div className="flex gap-1">
          {(
            [
              ["fit", "Network"],
              ["city", "City"],
              ["centre", "Centre"],
            ] as const
          ).map(([id, label]) => (
            <button key={id} type="button" onClick={() => props.onCamera(id)} className="flex-1 rounded bg-[#232920] px-2 py-1.5 text-sm text-[#d9d3c6]">
              {label}
            </button>
          ))}
        </div>
        <p className="font-mono text-[0.68rem] text-[#8d8678]">
          view {props.viewFps} fps · stream {props.streamFps} fps
          {header ? ` · spawned ${format(header.spawned)}` : ""}
        </p>
        <Legend />
      </aside>
      <section className="absolute bottom-3 left-4 right-4 z-20 flex items-center gap-3 rounded-md border border-[#2a3128] bg-[#161c14]/95 px-3 py-2 shadow-2xl backdrop-blur">
        <button
          type="button"
          onClick={props.onTogglePause}
          disabled={!props.desktop || !props.running}
          className="rounded bg-[#d7c4a1] px-3 py-1.5 text-sm font-semibold text-[#1b160f] disabled:opacity-50"
        >
          {paused ? "Play" : "Pause"}
        </button>
        <div className="w-24 shrink-0">
          <p className="font-mono text-lg text-[#f3efe4]">{clockLabel(time)}</p>
          <p className="text-[0.68rem] tracking-wide text-[#a89878] uppercase">{seeking ? "Seeking" : paused ? "Paused" : `${props.speed}×`}</p>
        </div>
        <input
          type="range"
          min={0}
          max={86400}
          step={1}
          value={Math.min(86400, Math.max(0, time))}
          disabled={!props.desktop || !props.running}
          onPointerDown={() => props.onScrub(time, false)}
          onChange={(event) => props.onScrub(Number(event.target.value), false)}
          onPointerUp={(event) => props.onScrub(Number((event.target as HTMLInputElement).value), true)}
          onKeyUp={(event) => props.onScrub(Number((event.target as HTMLInputElement).value), true)}
          className="min-w-0 flex-1 accent-[#d7c4a1] disabled:opacity-50"
          aria-label="Simulation time"
        />
        <div className="flex shrink-0 gap-1">
          {SPEEDS.map((value) => (
            <button
              key={value}
              type="button"
              disabled={!props.desktop || !props.running}
              onClick={() => props.onSpeed(value)}
              className={`rounded px-1.5 py-1 font-mono text-xs ${props.speed === value ? "bg-[#d7c4a1] text-[#1b160f]" : "bg-[#232920] text-[#d9d3c6]"} disabled:opacity-50`}
            >
              {value}×
            </button>
          ))}
        </div>
      </section>
    </>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-xs text-[#8d8678]">{label}</dt>
      <dd className="font-mono text-[#f3efe4]">{value}</dd>
    </div>
  );
}

function Sparkline({ samples }: { samples: number[] }) {
  const width = 280;
  const height = 48;
  const max = Math.max(20, ...samples);
  const points = samples
    .map((sample, index) => {
      const x = samples.length === 1 ? 0 : (index / (samples.length - 1)) * width;
      const y = height - (sample / max) * (height - 4) - 2;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg viewBox={`0 0 ${width} ${height}`} className="h-12 w-full rounded bg-[#10140f]" role="img" aria-label="Mean speed">
      {samples.length > 1 && <polyline fill="none" stroke="#3dbe6e" strokeWidth="2" points={points} />}
    </svg>
  );
}

function Legend() {
  const rows: Array<[string, string]> = [
    ["#3dbe6e", "Free-flow"],
    ["#e2a23a", "Moderate"],
    ["#d24b3a", "Heavy"],
    ["#6e1e33", "Standstill"],
  ];
  return (
    <ul className="grid grid-cols-2 gap-1 text-xs text-[#d9d3c6]">
      {rows.map(([color, label]) => (
        <li key={label} className="flex items-center gap-2">
          <span className="inline-block h-2.5 w-6 rounded-sm" style={{ background: color }} />
          {label}
        </li>
      ))}
    </ul>
  );
}

function format(value: number) {
  return new Intl.NumberFormat("en-US").format(value);
}
