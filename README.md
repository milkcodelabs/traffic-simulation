# Traffic Simulation

Research-grade mesoscopic urban traffic simulation for desktop study of a whole city. The engine is Rust. The shell is [Tauri](https://v2.tauri.app/) 2. The map is [MapLibre](https://maplibre.org/) GL JS with [deck.gl](https://deck.gl/) congestion, queue, and scenario-diff layers.

Thessaloniki, Greece, is the calibration network. A compiled graph ships with the repository and with the desktop bundle, so the first launch draws that city without an Overpass query.

## Key capabilities

- **Single-reservoir spatial queue.** Each directed link stores vehicles up to \(S_{\max,e} = \mathrm{lanes}_e \times K_{\mathrm{jam}} \times L_e\). Discharge after free-flow time is a capacity-limited server. Inflow stops when the reservoir is full, so a jam spills upstream without deleting vehicles. Capacity is taken from a triangular (Newell/Daganzo), Greenshields, or Smulders fundamental diagram at compile time.
- **Signal control, including night flashing amber.** Fixed cycles use green, a 3 s yellow, and a 1.5 s all-red. From 00:00 to 06:00 the compiled plans switch to flashing amber. Minor approaches then take a gap-acceptance delay \(\tau_{\mathrm{yield}} = 4.5\,\mathrm{s}\), and they yield while a major-road vehicle is ready. The mode change does not teleport vehicles.
- **Bounded-rationality dynamic traffic assignment.** Time-dependent shortest paths use the link weight \(t_e = \max(L_e / v_e,\, N_e / q_e)\), which is free-flow time plus queue delay. A driver keeps the current continuation until experienced time exceeds \(1.4\times\) its free-flow time.
- **A/B scenario differential.** A baseline run and an intervention run are compared link by link. The map paints improved links blue and degraded links red. Speed changes inside 5% of free-flow and delay changes inside 1 s stay neutral.
- **Headless Monte Carlo.** `sim-cli experiment --headless` runs the paired signal experiment on a Rayon pool and writes means, unbiased variances, and Student-t 95% intervals for vehicle-hours, vehicle-kilometres, and delay.

The supply model is a spatial queue, not a Godunov discretization of the Lighthill–Whitham–Richards equation. The limits of that choice are measured in [docs/SCIENTIFIC_AUDIT.md](docs/SCIENTIFIC_AUDIT.md) and written up in [docs/METHODOLOGY.md](docs/METHODOLOGY.md).

## Empirical benchmarks

Measured on 12 October 2026 with `sim-cli benchmark` (release profile: `opt-level = 3`, thin LTO, `codegen-units = 1`) on a 12th Gen Intel Core i7-1270P, 16 logical processors, 31.6 GiB RAM. The network is `data/cache/thessaloniki.simnet`: 16,937 nodes, 38,313 links, 2,009 signals. Full tables are in [docs/PERFORMANCE_REPORT.md](docs/PERFORMANCE_REPORT.md).

| Result | Value |
| --- | ---: |
| 24-hour run, 100,000 requested boundary vehicles | 81,639 vehicles actually spawned |
| Wall time for that day | 362 s (6.03 min) |
| Throughput | 239× real time |
| Peak resident set | 161 MiB |
| Cache load | 28 ms, 15.2 MiB file, 31.8 MiB resident once the empty stepper is up |

Playback at interactive rates fits the same laptop for the loads in the report. Throughput stops rising after a small thread count because departure routing and the per-tick conservation walk stay serial.

## Quickstart

### Prerequisites

- Rust 1.75 or newer (`rustup` stable). The performance numbers above were produced with rustc 1.99.0.
- Node.js 18 or newer.
- Tauri’s operating-system dependencies:
  - **Windows:** WebView2 (already present on current Windows 10 and 11) and the Microsoft C++ build tools.
  - **macOS:** Xcode command-line tools.
  - **Linux:** a WebKitGTK 4.1 development package, plus the GTK and related libraries listed in the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/).

Install the Tauri CLI once if `cargo tauri` is not already on the path:

```text
cargo install tauri-cli --version "^2.0.0"
```

The repository root also depends on `@tauri-apps/cli`, so `npm install` followed by `npm run tauri -- dev` is the same entry point.

### Desktop application

From the repository root:

```text
npm install
npm install --prefix ui
cargo tauri dev
```

`cargo tauri build` produces the installer and the release executable. The bundle contains `data/cache/thessaloniki.simnet` and the two scenario files in `data/presets/`. On startup the shell calls `compiled_cache`, loads that graph, and opens the simulation. It does not download a map.

A release build uses the workspace profile already set in `Cargo.toml`:

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
```

### Headless CLI

Commands below assume the working directory is the repository root. `cargo run -p sim-cli --release --` can replace a bare `sim-cli` if the binary is not on `PATH`.

Compile or refresh a network. The preset reads a cached raw extract when one exists; `--refresh` hits Overpass again.

```text
sim-cli ingest --preset thessaloniki
sim-cli ingest --bbox 40.57,22.885,40.675,23.015
sim-cli ingest --osm path/to/city.osm.pbf
```

Inspect the bundled graph, report short-link Courant numbers, run one scenario, and run the paired Monte Carlo:

```text
sim-cli inspect --cache data/cache/thessaloniki.simnet
sim-cli audit
sim-cli run --scenario data/presets/thessaloniki_baseline.json --horizon-s 3600 --daily-vehicles 6000
sim-cli experiment --headless --runs 10
```

`experiment` is the batch runner (`batch` is an alias). The default experiment is 6,000 boundary vehicles over 24 hours, ten paired seeds, written to `data/experiments/phase4/`. The 239× figure is not that default. It is the benchmark day:

```text
sim-cli benchmark --only day
```

`--only` also accepts `static`, `loads`, `scale`, and `all`. `all` is a long job: three loads, several thread counts, and a full day.

## Documentation

| Document | What it covers |
| --- | --- |
| [docs/METHODOLOGY.md](docs/METHODOLOGY.md) | Supply model, fundamental diagrams, grade, signals, assignment, and the discretization limits |
| [docs/SCIENTIFIC_AUDIT.md](docs/SCIENTIFIC_AUDIT.md) | Measured invariants, the conditional-validity verdict, and the routing fixes |
| [docs/PERFORMANCE_REPORT.md](docs/PERFORMANCE_REPORT.md) | Host, protocol, tick scaling, memory, and the 24-hour run |
| [docs/RESEARCH_ROADMAP.md](docs/RESEARCH_ROADMAP.md) | What this supply model can support next, and what would replace it |
| [docs/CLI_REFERENCE.md](docs/CLI_REFERENCE.md) | `ingest`, `inspect`, `run`, `batch`, `experiment`, `benchmark`, and `audit` |
| [docs/DATA_SCHEMAS.md](docs/DATA_SCHEMAS.md) | Scenario JSON, experiment CSV, and the `0x54534631` frame |

## Repository layout

```text
crates/sim-core     compiled graph, diagrams, signals, scenarios, .simnet cache
crates/sim-engine   spatial-queue step, routing, Monte Carlo
crates/sim-ingest   OSM XML/PBF, presets, contraction
crates/sim-ipc      binary viewport, frame, and geometry
crates/sim-cli      headless ingest, experiments, and benchmarks
src-tauri           desktop shell
ui                  React map and telemetry
data/cache          thessaloniki.simnet (tracked)
data/presets        baseline and night-amber scenario JSON
data/raw            ignored OSM extracts
```

## Citation

```bibtex
@software{milk2026traffic,
  author  = {Milk},
  title   = {Traffic Simulation: A Mesoscopic Urban Traffic Engine},
  year    = {2026},
  version = {1.0.0},
  license = {MIT},
  note    = {Spatial-queue simulator. Thessaloniki is the bundled calibration network.}
}
```

## License

MIT. See [LICENSE](LICENSE).
