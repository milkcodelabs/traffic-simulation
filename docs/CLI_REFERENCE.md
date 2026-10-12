# CLI reference

`sim-cli` compiles a road network and runs headless experiments against a `.simnet` cache. The binary is the `sim-cli` package in this workspace.

```text
cargo run -p sim-cli --release -- <command> [options]
```

Release timings, including the benchmark in [PERFORMANCE_REPORT.md](PERFORMANCE_REPORT.md), require `--release`. A debug build prints a warning from `benchmark` and must not be quoted as a result.

The default city is Thessaloniki. `data/cache/thessaloniki.simnet` is tracked. Commands that take `--preset thessaloniki` open that file and do not call Overpass.

Global behavior: a failure prints `error: ...` and exits 1.

## ingest

Download or read a road network and write a `.simnet` cache.

```text
sim-cli ingest [--preset <name>] [--osm <file>] [--bbox <min_lat,min_lon,max_lat,max_lon>]
               [--cache-dir <dir>] [--refresh]
```

| Flag | Meaning |
| --- | --- |
| `--preset <name>` | Named city. `thessaloniki` is the only built-in preset. Conflicts with `--osm` and `--bbox`. |
| `--osm <file>` | Local `.osm`, `.xml`, or `.pbf`. Conflicts with `--preset` and `--bbox`. |
| `--bbox` | `min_lat,min_lon,max_lat,max_lon`. Conflicts with `--preset` and `--osm`. |
| `--cache-dir` | Directory that will contain `cache/` and, for downloads, `raw/`. Default: the workspace `data/` directory, or a `data/` directory beside the executable when the repo is not found. |
| `--refresh` | Ignore a saved Overpass extract and download again. |

If none of `--preset`, `--osm`, and `--bbox` is set, the command ingests the Thessaloniki preset.

Overpass is used only for `--preset` and `--bbox`, and only when `data/raw/<name>.osm.xml` is missing or `--refresh` is set. The raw extract matches `data/raw/*.osm*` in `.gitignore`. The compiled cache is `<cache-dir>/cache/<name>.simnet`.

Stdout reports the cache path, node and edge counts, length, contractions, the diagram name, and the weak-component census. Warnings are printed one per line.

## inspect

Print the counts stored in a cache. `summary` is a visible alias.

```text
sim-cli inspect --cache <file.simnet>
sim-cli summary --cache <file.simnet>
```

| Flag | Meaning |
| --- | --- |
| `--cache` | Required path of a `.simnet` file. |

Stdout is one line: nodes, edges, length in kilometres, signals, and diagram name. This command does not simulate.

## run

Advance one scenario file over one horizon and print network totals.

```text
sim-cli run [--cache <file>] [--preset thessaloniki]
            --scenario <file.json>
            [--daily-vehicles 6000] [--horizon-s 86400] [--seed 1]
            [--output <dir>]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--scenario` | required | Scenario JSON. The bundled files are `data/presets/thessaloniki_baseline.json` and `data/presets/thessaloniki_night_amber.json`. |
| `--cache` | from `--preset` | Compiled network. |
| `--preset` | `thessaloniki` | Used when `--cache` is omitted. |
| `--daily-vehicles` | 6000 | Boundary vehicles over the horizon, shaped by the diurnal profile. Not an origin–destination matrix. |
| `--horizon-s` | 86400 | Simulation horizon in seconds. |
| `--seed` | 1 | Departure seed. |
| `--output` | omitted | Directory. When set, writes `run.csv` with one data row. |

Stdout:

```text
scenario <name>  seed <n>  vht_h <h>  vkt_km <km>  delay_h <h>  spawned <n>  completed <n>
```

A 24-hour city run is minutes of wall time. Pass a shorter `--horizon-s` for a smoke test. The published 81,639-vehicle day is `benchmark --only day`, which requests 100,000 boundary vehicles, not this 6,000 default.

The run refuses to return totals if `spawned != active + completed` or if VHT, VKT, delay, or mean speed is non-finite.

## experiment

Paired Monte Carlo: fixed signals for the whole day against night flashing amber from 00:00 to 06:00. `batch` is a visible alias. The two names run the same function.

```text
sim-cli experiment --headless [--runs 10] [--cache <file>] [--preset thessaloniki]
                   [--output data/experiments/phase4]
                   [--daily-vehicles 6000] [--horizon-s 86400] [--seed 1]

sim-cli batch --headless --runs 10
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--headless` | off | Required. Without it the command exits with an error. The batch is CPU-side; there is no window. |
| `--runs` | 10 | Replicates per arm. Arms run together on a Rayon pool. |
| `--cache` | from `--preset` | Compiled network. |
| `--preset` | `thessaloniki` | Used when `--cache` is omitted. |
| `--output` | `data/experiments/phase4` | Directory for `summary.csv`, `runs.csv`, `links.csv`, `diff.bin`, and `scenarios.json`. |
| `--daily-vehicles` | 6000 | Boundary vehicles over the horizon, diurnal. |
| `--horizon-s` | 86400 | Horizon in seconds. |
| `--seed` | 1 | Base seed. Arm 0 run \(r\) uses `seed + r`. Arm 1 uses `seed + (1 << 32) + r`. |

The baseline arm is `baseline-24h-signals` (`signalSchedule = forceFixed`). The intervention arm is `night-flashing` (schedule unchanged, night window 00:00–06:00). Those are the same policies as the two files in `data/presets/`, under the names the batch writer uses.

Progress is printed to stderr per finished job and per simulated hour. Stderr ends with the output directory, the wall time, and the improved / degraded / neutral link counts.

## benchmark

Measure load time, tick throughput, memory, scaling, and frame encoding.

```text
sim-cli benchmark [--cache <file>] [--preset thessaloniki]
                  [--only all|loads|scale|day|static]
                  [--threads <n>] [--vehicles <n>] [--measure-ticks <n>]
                  [--warmup-s <s>] [--daily-vehicles <n>] [--horizon-s <s>]
                  [--seed 1] [--output docs/perf-results.json]
                  [--worker tick|day]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--only` | `all` | `static` measures load and frame size. `loads` times the three vehicle counts on the native thread count. `scale` times them across 1, 2, 4, 8, and 16 threads (plus the native count if it is not in that list). `day` is the 24-hour run. `all` is static, scale, and day. |
| `--threads` | native width, or the scale plan | Fixes the pool width. A scale sweep is replaced by this single width. |
| `--vehicles` | omitted | Time one load instead of the plan. The suite still uses `--measure-ticks` and `--warmup-s`. |
| `--measure-ticks` | 90 | Timed `step(1.0)` calls after warmup. |
| `--warmup-s` | 45 | Simulation seconds advanced before the timed window. |
| `--daily-vehicles` | 100000 | Boundary demand for `--only day`. |
| `--horizon-s` | 86400 | Horizon for the day worker. |
| `--seed` | 1 | Day-run seed. |
| `--output` | `docs/perf-results.json` | JSON document. The file is rewritten as each stage finishes. |
| `--worker` | omitted | `tick` or `day`. The parent process spawns itself with this flag. Callers do not set it. |

Load tests release the trips at \(t = 0\), walk the warmup, and time only the following window. Signals in those tests are forced to the fixed cycle with the clock origin at 08:00. The day run uses compiled plans and flashing amber from 00:00 to 06:00. Each thread count is a separate process so the Rayon pool is built at that width.

The 12 October 2026 day result on the i7-1270P is 81,639 spawned vehicles, 362 s, 239× real time, 161 MiB peak resident. See [PERFORMANCE_REPORT.md](PERFORMANCE_REPORT.md).

## audit

Report link length and the Courant number \(v_f \Delta t / L\) on a compiled cache, with \(\Delta t = 1\,\mathrm{s}\) and \(v_f\) the compiled free-flow speed.

```text
sim-cli audit [--cache <file>] [--preset thessaloniki]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--cache` | from `--preset` | Compiled network. |
| `--preset` | `thessaloniki` | Used when `--cache` is omitted. |

Stdout includes nodes, edges, signals, length, diagram, minimum link length, maximum Courant number, and the count of links with Courant number above 1. The command does not step the simulation.

The invariant suite that checks conservation, spillback, signals, and bit-level repeat is separate:

```text
cargo test -p sim-engine --test scientific_audit
```

That harness is what [SCIENTIFIC_AUDIT.md](SCIENTIFIC_AUDIT.md) records. `sim-cli audit` is the geometric summary a cache can answer without replaying the fixtures.

## Exit status

| Code | When |
| --- | --- |
| 0 | The command finished and wrote its report. |
| 1 | Unknown preset, unreadable cache, refused bbox, missing `--headless`, conservation failure, or an I/O error. The message is on stderr. |
