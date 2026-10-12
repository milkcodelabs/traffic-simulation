# Data schemas

Versions below match Traffic Simulation 1.0.0. Multi-byte numbers are little-endian.

## Scenario JSON

A scenario is the `Scenario` struct in `crates/sim-core/src/scenario.rs`, serialized with camelCase fields. Geometry is not stored in the file. Scales refer to edge and zone ids in the compiled network.

```json
{
  "name": "thessaloniki-baseline",
  "signalSchedule": "forceFixed",
  "nightFlashing": null,
  "edgeCapacityScale": [],
  "edgeSpeedScale": [],
  "zoneDemandScale": [],
  "globalSpeedScale": 1.0,
  "globalDemandScale": 1.0,
  "routeInertia": 1.4
}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `name` | string | Label copied into logs and `run.csv`. |
| `signalSchedule` | `unchanged`, `forceFixed`, or `forceFlashingAmber` | Overrides the controller clock. `forceFixed` holds the fixed cycle for the whole run. |
| `nightFlashing` | object or null | `{ "startS": <u32>, "endS": <u32> }` seconds from midnight, end exclusive. The night preset uses `0` and `21600` (00:00–06:00). Null leaves each controller’s own night window in charge when the schedule is `unchanged`. |
| `edgeCapacityScale` | array of `{ "edge": <u32>, "scale": <f64> }` | Multiplies that edge’s capacity. Scales on the same edge stack. Zero closes the link. Negative values are clamped to zero. |
| `edgeSpeedScale` | array, same shape | Multiplies that edge’s free-flow speed, before `globalSpeedScale`. |
| `zoneDemandScale` | array of `{ "zone": <u32>, "scale": <f64> }` | Multiplies boundary demand associated with that zone. |
| `globalSpeedScale` | number | Applied after per-edge speed scales. `1` leaves posted speeds alone. |
| `globalDemandScale` | number | Multiplies the boundary departure rate. This is not an origin–destination matrix. |
| `routeInertia` | number | Reroute only when experienced continuation time exceeds this times free-flow time. The presets use `1.4`. |

Bundled files:

| File | Policy |
| --- | --- |
| `data/presets/thessaloniki_baseline.json` | `forceFixed`, no extra night window. Standard 24-hour fixed cycle. |
| `data/presets/thessaloniki_night_amber.json` | `unchanged`, `nightFlashing` from 0 to 21600. Flashing amber 00:00–06:00, fixed cycle after 06:00. |

`sim-cli experiment` does not read these files. It builds the same two policies in code and writes a short descriptor, `scenarios.json`, into the output directory:

```json
{
  "baseline": { "name": "baseline-24h-signals", "signalSchedule": "forceFixed" },
  "intervention": {
    "name": "night-flashing",
    "signalSchedule": "unchanged",
    "nightFlashing": { "startS": 0, "endS": 21600 }
  },
  "demand": "diurnal boundary vehicles at compiled gateways, not an origin-destination matrix"
}
```

## Experiment CSV

Written by `sim-cli experiment` (alias `batch`) into `--output`.

### summary.csv

One row per arm and metric.

```text
arm,metric,n,mean,variance,ci95_low,ci95_high
```

| Column | Meaning |
| --- | --- |
| `arm` | `baseline-24h-signals` or `night-flashing`. |
| `metric` | `vht_h`, `vkt_km`, or `delay_h`. Hours and kilometres, not seconds and metres. |
| `n` | Replicate count. |
| `mean` | Sample mean. |
| `variance` | Unbiased sample variance (divisor \(n - 1\)). Zero when \(n < 2\). |
| `ci95_low`, `ci95_high` | Two-sided Student-t 95% interval for the mean. Degrees of freedom above 30 use 1.96. Ten runs (9 degrees of freedom) use 2.262. |

### runs.csv

One row per seed.

```text
arm,seed,vht_s,vkt_m,delay_s,spawned,completed
```

`vht_s`, `vkt_m`, and `delay_s` are seconds and metres. `seed` is the integer actually used for that replicate.

### links.csv

One row per directed edge, in dense edge-index order.

```text
edge,base_delay_s,test_delay_s,delta_delay_s,base_speed_mps,test_speed_mps,delta_speed_mps,class
```

| Column | Meaning |
| --- | --- |
| `edge` | Index into the compiled edge array. |
| `base_*` | Mean over baseline replicates. |
| `test_*` | Mean over the night-flashing replicates. |
| `delta_delay_s` | `test_delay_s - base_delay_s`. |
| `delta_speed_mps` | Empty when either arm never observed a speed. Otherwise `test - base`. |
| `class` | `0` neutral, `1` improved, `2` degraded. |

Speed cells are empty when the mean is non-finite (no observation). Classification follows [METHODOLOGY.md](METHODOLOGY.md): a speed change inside 5% of free-flow and a delay change inside 1 s is neutral. The score \(\Delta v / v_{\mathrm{free}} - \Delta \mathrm{Delay} / 30\) decides the colour when either band is exceeded. Positive is improved.

### run.csv

Written by `sim-cli run --output <dir>` only.

```text
scenario,seed,horizon_s,daily_vehicles,vht_s,vkt_m,delay_s,spawned,completed
```

## diff.bin

Headless comparison packet, little-endian, magic `TSD1` (`0x54534431`).

```text
magic            u32   0x54534431
edge_count       u32
improved         u32
degraded         u32
neutral          u32
baseline_time_s  f64   NaN in the headless file (the arms are finished runs, not a live clock)
compared_time_s  f64   0 in the headless file
class            u8 × edge_count    0 neutral, 1 improved, 2 degraded
```

The desktop `simulation_diff` packet uses the same 36-byte header and the same class bytes, then appends four `f64` values: baseline VHT, baseline delay, compared VHT, compared delay. `baseline_time_s` is NaN until a baseline has been captured. The webview treats a non-finite time or metric as “not yet recorded”.

## Simulation frame (`0x54534631`)

Published by the desktop session and decoded in `ui/src/frame.ts`. Magic `TSF1` is the integer `0x54534631`. Version is 1. The header is 80 bytes.

```text
offset  size  field
0       u32   magic = 0x54534631
4       u32   version = 1
8       u64   sequence
16      f64   time_s          seconds since the run origin
24      f32   speed           playback multiplier
28      u32   flags           bit 0 paused, bit 1 seeking, bit 2 unchanged
32      f32   mean_speed_mps
36      f64   vht_s
44      f64   vkt_m
52      f64   delay_s
60      u32   spawned
64      u32   active
68      u32   completed
72      u32   edge_count
76      u32   bottleneck_count
```

After the header:

```text
bottleneck_count × (edge u32, speed_ratio f32, queue_veh f32)
edge_count × speed_ratio f32
edge_count × density_ratio f32
edge_count × queue_veh f32
```

Edge order is the dense edge index, the same order as the viewport blob. A bottleneck row is present only when `queue_veh > 0`, at most eight, highest queue first, then lower speed ratio, then lower edge index.

When link contents are unchanged, the publisher sets flag bit 2 and writes `edge_count = 0` and `bottleneck_count = 0` with no columns (80 bytes total). The webview keeps the previous speed, density, and queue arrays and still reads the clock from the header.

A full frame for 38,313 edges and 8 bottleneck rows is 459,932 bytes, as measured in the performance report.

`speed_ratio` is observed speed over free-flow speed. The map paints it green / orange / red / dark burgundy. `density_ratio` is occupancy over storage. `queue_veh` is the stopped count used by the queue layer.

## Viewport blob

Returned by `network_geometry`. Magic `0x54534E31`. This blob is geometry for drawing. Simulation state is not in it.

```text
magic            u32 = 0x54534E31
edge_count       u32
repeated edge_count times:
  edge_index     u32
  class          u8
  lanes          u8
  speed_kph      u16
  point_count    u16
  point_count × (lon f32, lat f32)
```

## Network cache (`.simnet`)

```text
magic     4 bytes  ASCII "TSIM"
version   u32      1
payload   bincode-encoded Network
```

`data/cache/thessaloniki.simnet` is the tracked Thessaloniki graph (about 15.2 MiB). The desktop bundle copies it to `data/cache/thessaloniki.simnet` inside the Tauri resource directory. Loading that file does not contact Overpass. A new city is produced with `sim-cli ingest` and is gitignored by the `*.simnet` rule; the Thessaloniki file is the exception.
