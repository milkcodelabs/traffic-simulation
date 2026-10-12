# Performance Report

Measured on 12 October 2026 from `sim-cli benchmark` (release build) against `data/cache/thessaloniki.simnet`. Raw records are in `docs/perf-results.json`.

The binary was built with the workspace release profile: `opt-level = 3`, thin LTO, `codegen-units = 1`. `debug_assertions` was off. Compiler: `rustc 1.99.0 (b940084d7 2026-09-28)`.

## Host

| Item | Value |
| :--- | :--- |
| CPU | 12th Gen Intel Core i7-1270P |
| Cores / logical processors | 12 / 16 |
| Base clock reported by the firmware | 2200 MHz |
| RAM | 33,975,820,288 bytes (31.6 GiB), of which 17.9 GiB was free at the start |
| OS / arch | Windows x86_64 |

Every thread count below is a separate process. The Rayon pool is built at that width before the first step. Tick time is `Simulation::step(1.0)` only. Stage timers are the stepper's own clocks over the same window.

## 1. Executive Summary & Efficiency Verdict

The compiled Thessaloniki network is 16,937 nodes, 38,313 edges, 94,460 turns, and 2,009 signals (2,373 km, 2,763 lane-km, 367,016 vehicles of storage). Loading the 15.2 MiB cache takes 28 ms. An empty stepper adds 2.5 ms and brings the process to 31.8 MiB resident.

On this laptop the engine is fast enough for interactive desktop use and for single-city academic runs. Steady-state ticks allocate on every step, and throughput stops rising after two threads.

- **3,480 active vehicles** (night / taper band): 2.14 ms per tick on one thread (466× real time) and 0.74 ms on two threads (1,349×).
- **29,960 active vehicles** (peak band): 10.4 ms on one thread (96×) and 6.35 ms on 16 threads (158×). Playback at 100× fits in the 8-thread measurement (147×).
- **79,959 active vehicles** (saturation): 34.2 ms on one thread (29×) and 20.4 ms at the best width, two threads (49×). A 20× interactive playback fits inside that 20.4 ms tick. 100× playback would need a tick under 10 ms.
- **One 24-hour day** at 100,000 requested boundary vehicles (81,639 actually spawned) finishes in **362 s (6.03 min), 239× real time**, peak resident set **161 MiB**.

Every measured tick allocates roughly once per active vehicle (0 of 1,350 timed ticks were allocation-free). Past two threads, gridlock throughput stays near 46×. The 24-hour run spends its wall time in serial shortest-path searches at departure.

For large-scale academic work on one city, the current build is usable on a laptop: a heavy day takes 6 minutes and the working set stays under 200 MiB. A 50-run Monte Carlo of that same heavy day is a workstation job of tens of minutes. The two changes that would move those numbers are dropping the per-tick path clone and parallelizing departure routing.

## 2. Empirical Benchmark Data & Scaling Tables

### Protocol

1. Schedule N routable gateway trips at t = 0. The release step routes them serially and is timed separately from the steady window.
2. Advance the clock to t = 45 s so the first links can discharge.
3. Time 90 calls of `step(1.0)`. All 90 ticks were busy in every run.
4. Signals in the load tests are forced to fixed cycle with the clock origin at 08:00.
5. The 24-hour run uses the diurnal boundary instrument, seed 1, compiled plans, and flashing amber from 00:00 to 06:00. Thread count 16.

Scheduled counts were 3,500, 30,000, and 80,000. Active counts at the start of the window were 3,480, 29,960, and 79,959. Four trips in the small load, and 40 in the larger loads, had completed or never entered before the window; rejected departures during release were zero.

Vehicles do not spread evenly. On-link counts saturate near 3,300 while the rest wait upstream of full entry links:

| Load | Active | On links | Waiting | Full edges | Max queue | Max spillback | Peak density |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Low | 3,480 | 2,491 | 989 | 171 | 53 | 2,352 | 0.999 |
| Peak | 29,960 | 3,196 | 26,764 | 345 | 111 | 28,545 | 0.999 |
| Gridlock | 79,959 | 3,299 | 76,660 | 358 | 111 | 78,506 | 0.999 |

The 80,000-vehicle case is gateway spillback against a cordon that already holds about 3,300 vehicles, not a network filled to its 367,016-vehicle storage ceiling.

### Tick throughput vs. active load

Mean is over 90 ticks. Real-time speedup equals ticks per second because Δt = 1 s.

| Load | Threads | Active | Mean tick (ms) | p50 (ms) | p95 (ms) | Ticks/s | Real-time speedup |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Low | 1 | 3,480 | 2.145 | 2.044 | 3.826 | 466.2 | 466× |
| Low | 2 | 3,480 | 0.741 | 0.678 | 1.256 | 1,348.9 | 1,349× |
| Low | 4 | 3,480 | 0.757 | 0.680 | 1.231 | 1,320.8 | 1,321× |
| Low | 8 | 3,480 | 0.863 | 0.680 | 1.792 | 1,158.7 | 1,159× |
| Low | 16 | 3,480 | 0.813 | 0.582 | 2.179 | 1,229.2 | 1,229× |
| Peak | 1 | 29,960 | 10.392 | 9.473 | 15.278 | 96.2 | 96× |
| Peak | 2 | 29,960 | 13.460 | 13.566 | 15.027 | 74.3 | 74× |
| Peak | 4 | 29,960 | 7.983 | 8.109 | 9.371 | 125.3 | 125× |
| Peak | 8 | 29,960 | 6.794 | 6.860 | 7.936 | 147.2 | 147× |
| Peak | 16 | 29,960 | 6.348 | 6.229 | 7.623 | 157.5 | 158× |
| Gridlock | 1 | 79,959 | 34.195 | 34.220 | 44.751 | 29.2 | 29× |
| Gridlock | 2 | 79,959 | 20.411 | 20.233 | 22.529 | 49.0 | 49× |
| Gridlock | 4 | 79,959 | 21.855 | 22.119 | 24.765 | 45.8 | 46× |
| Gridlock | 8 | 79,959 | 22.095 | 22.175 | 24.947 | 45.3 | 45× |
| Gridlock | 16 | 79,959 | 21.627 | 21.418 | 25.009 | 46.2 | 46× |

Release routing, excluded from the table above, is serial Dijkstra at departure:

| Trips | Wall time across the five processes | Per search |
| :--- | :--- | :--- |
| 3,500 | 2.66–3.23 s | 0.76–0.92 ms |
| 30,000 | 22.3–35.4 s | 0.74–1.18 ms |
| 80,000 | 57.7–80.1 s | 0.72–1.00 ms |

The giant component used by the router is 16,618 nodes. A search on it costs about a millisecond on this CPU.

### Subsystem breakdown

Shares below are the one-thread runs. On one thread the stage timers partition the step. At higher widths the routing timer sums per-thread search clocks, so those percentages are not a wall-time split.

Signal time is discharge-credit evaluation inside each busy node. There is no separate controller sweep. "Apply" is the serial commit inside the link bucket.

| Load (1 thread) | Link dynamics | of which apply | Signals | Routing | Metrics / audit | Accounted stage time |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| Low, 3,480 active | 75.6% | 6.0% | 8.6% | 12.0% | 3.9% | 187 ms / 90 ticks |
| Peak, 29,960 active | 76.0% | 1.3% | 1.7% | 2.7% | 19.5% | 926 ms / 90 ticks |
| Gridlock, 79,959 active | 76.1% | 0.6% | 0.7% | 1.1% | 22.0% | 3,063 ms / 90 ticks |

Mean busy nodes were 489 (low), 669 (peak), and 684 (gridlock). The parallel region is the per-node proposal. The serial tail that grows with spillback is `note_saturation` plus the conservation audit, which walk every waiting vehicle. That tail is 4% of a small load and 22% of the 80,000-vehicle load.

Reroutes inside the 90-tick window were 107, 192, and 262. The cooldown is 90 s, so almost every path clone in the window is a copy of a path that is kept.

### Multi-thread scaling

Speedup is ticks/s divided by the one-thread ticks/s of the same load. Efficiency is speedup divided by thread count.

| Threads | Low ticks/s | Low speedup | Low efficiency | Peak ticks/s | Peak speedup | Peak efficiency | Gridlock ticks/s | Gridlock speedup | Gridlock efficiency |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 466.2 | 1.00× | 100% | 96.2 | 1.00× | 100% | 29.2 | 1.00× | 100% |
| 2 | 1,348.9 | 2.89× | 145% | 74.3 | 0.77× | 39% | 49.0 | 1.68× | 84% |
| 4 | 1,320.8 | 2.83× | 71% | 125.3 | 1.30× | 33% | 45.8 | 1.57× | 39% |
| 8 | 1,158.7 | 2.49× | 31% | 147.2 | 1.53× | 19% | 45.3 | 1.55× | 19% |
| 16 | 1,229.2 | 2.64× | 16% | 157.5 | 1.64× | 10% | 46.2 | 1.58× | 10% |

The low-load step from one thread to two is above linear in this sample (p50 moves from 2.04 ms to 0.68 ms, not only the mean). Throughput then stops rising. On the gridlock load the best width is two threads. Sixteen threads is within a few percent of four threads and well below the Amdahl line implied by the one-thread split.

Taking the gridlock one-thread split, the parallel proposal (link dynamics without apply, plus in-proposal signals and reroutes) is 77% of the tick and the serial apply-plus-audit tail is 23%. Amdahl's law at 16 threads allows about 3.6×. The measured speedup is 1.58×. The same gap appears on the peak load (stage split allows about 3.9×, measurement is 1.64×). The missing factor is the allocator: the proposal builds new vectors and clones every path on every busy node, and those allocations sit on the parallel tasks.

### Memory footprint

Static model, counted from buffer capacities after `load_network`:

| Region | Bytes |
| :--- | ---: |
| Edge records (`Edge` is 224 bytes, geometry headers included) | 16,776,704 |
| Polyline coordinates | 5,427,168 |
| Turn records (94,460 × 24 bytes) | 4,194,240 |
| Node records (`Node` is 72 bytes) | 2,097,072 |
| Names, refs, diagram string | 755,279 |
| Adjacency CSR | 442,008 |
| OSM way ids | 321,768 |
| Signal plans (2,009 controllers) | 264,272 |
| Zones | 84,644 |
| Turn offsets | 67,752 |
| **Total retained graph** | **30,431,355** |

That is **794 bytes/edge** and **1,797 bytes/node**. The on-disk cache is 15,908,429 bytes and decodes in 27.6 ms. RSS moves from 5.7 MiB before the load, to 26.9 MiB with the graph resident, to 31.8 MiB after an empty `Simulation` is constructed (peak during that process 46.5 MiB). Empty stepper buffers are 5.07 MiB, of which 3.37 MiB is the per-edge state (cumulative counts, free-flow and experienced weights, discharge credits) and 1.22 MiB is the outer queue table.

Dynamic buffers at the start of the measured window:

| Load | Stepper buffers | Vehicle records | Path bytes | RSS at 1 thread | RSS at 16 threads |
| :--- | ---: | ---: | ---: | ---: | ---: |
| Low | 8.54 MiB | 0.47 MiB | 3.00 MiB | 46.6 MiB | 60.3 MiB |
| Peak | 35.8 MiB | — | — | 68.1 MiB | 95.2 MiB |
| Gridlock | 92.6 MiB | — | — | 122 MiB | 151 MiB |

`Vehicle` is 120 bytes. The 16-thread processes sit about 29 MiB above the one-thread processes on the same load, which is the worker-thread resident set on top of the same graph.

### Allocator behaviour

A counting allocator was armed only around the 90 timed steps. Steady state does **not** run without heap traffic.

| Load (1 thread) | Allocations / tick | Bytes / tick | Bytes / allocation | Ticks with zero allocations |
| :--- | ---: | ---: | ---: | ---: |
| Low | 3,143 | 734,229 | 234 | 0 / 90 |
| Peak | 28,936 | 17,929,330 | 620 | 0 / 90 |
| Gridlock | 78,868 | 52,200,939 | 662 | 0 / 90 |

Frees match allocations within a few dozen per tick, so this is churn, not a leak inside the window. The count tracks the active-vehicle count because `decide_next` and `decide_entry` clone `vehicle.path` on every visit, including visits that do not reroute. At the gridlock rate that is about 1.5 GB/s of allocate-and-free (52.2 MB × 29.2 ticks/s).

Edge state, queue indices, and the Dijkstra scratchpad are reused. The scratchpad on the thread that performed release is 1.15 MB (distances, predecessors, and the compact turn table). It is not rebuilt per search.

Completed vehicles are kept. After the 24-hour run the stepper held 96.2 MiB, of which 74.7 MiB was path storage and 15.7 MiB was `Vehicle` records for 81,639 spawns.

### Cache and locality

The hot link state (`cumulative_in`, `cumulative_out`, `cumulative_demand`, free-flow weights, experienced weights, discharge credits) is a set of contiguous `f64` arrays indexed by edge. A sequential scan of 38,313 `f64`s on this CPU costs **0.67 ns/element**.

`collect_busy` walks `occupied: Vec<u32>`, a dense list of edges that hold someone, then one `VecDeque` per occupied edge. A microbenchmark that touches the front of 38,313 separately allocated deques costs **2.07 ns/element, 3.1× the sequential scan**. Random gathers into the same `f64` array cost 0.62 ns/element: at this size the array fits in cache, and the penalty that shows up is the per-edge heap indirection, not a large-stride gather.

`network.edges[i]` is a 224-byte record that also owns polyline and name buffers. The discharge loop only needs length, capacity, storage, and the head node, so each touch pulls cold geometry headers onto the line. Vehicle paths are a second heap object beside the 120-byte record, so a queue of vehicle ids is a pointer chase into `Vehicle` and then another into `path`.

### 24-hour run

Demand was 100,000 boundary vehicles over 86,400 s, shaped by the urban diurnal profile, seed 1, 16 threads. The routability filter rejected 18,355 departures. The run spawned 81,639, completed 49,205, and finished with 32,434 still active (15,290 on links, 17,144 waiting).

| Result | Measured |
| :--- | :--- |
| Wall time | 362.1 s (6.03 min) |
| Real-time speedup | 239× |
| Steps / coasts | 86,367 steps in 354.5 s, 14 coasts in 7.4 s |
| Step throughput | 244 ticks/s over the whole day |
| Peak active | 32,434, at the end of the day |
| Peak RSS | 161.3 MiB (169,119,744 bytes); private commit 174 MiB |
| VHT | 274,464 vehicle-hours |
| VKT | 635,447 vehicle-km |
| Delay | 16,135 vehicle-hours |
| Day mean speed | 2.31 km/h (VKT / VHT) |
| Full edges / max queue / spillback | 1,732 / 125 / 17,144 |

The day mean speed is 2.31 km/h because the afternoon gridlocks. Night hours are a different regime. Cumulative wall time and the census at each hour:

| Hour | Active | On links | Waiting | RSS (MiB) | Cumulative wall (s) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 62 | 62 | 0 | 35.0 | 0.5 |
| 1 | 65 | 65 | 0 | 35.3 | 1.0 |
| 2 | 68 | 68 | 0 | 35.7 | 1.5 |
| 3 | 65 | 65 | 0 | 36.0 | 2.0 |
| 4 | 85 | 85 | 0 | 36.5 | 2.6 |
| 5 | 276 | 276 | 0 | 40.5 | 5.3 |
| 6 | 1,033 | 1,033 | 0 | 45.9 | 14.0 |
| 7 | 2,410 | 2,410 | 0 | 71.5 | 29.7 |
| 8 | 3,630 | 3,532 | 98 | 80.6 | 49.4 |
| 9 | 3,860 | 3,461 | 399 | 87.0 | 66.2 |
| 10 | 3,180 | 2,700 | 480 | 92.6 | 75.6 |
| 11 | 3,984 | 2,959 | 1,025 | 99.1 | 83.9 |
| 12 | 5,840 | 4,015 | 1,825 | 108.7 | 93.5 |
| 13 | 8,305 | 5,334 | 2,971 | 116.5 | 105.6 |
| 14 | 10,904 | 6,318 | 4,586 | 126.0 | 123.0 |
| 15 | 14,326 | 7,633 | 6,693 | 132.5 | 146.7 |
| 16 | 19,390 | 9,860 | 9,530 | 141.7 | 172.9 |
| 17 | 24,973 | 12,338 | 12,635 | 150.2 | 201.0 |
| 18 | 28,937 | 14,085 | 14,852 | 156.0 | 235.7 |
| 19 | 30,980 | 14,826 | 16,154 | 159.4 | 266.1 |
| 20 | 31,688 | 15,038 | 16,650 | 160.7 | 295.6 |
| 21 | 31,992 | 15,134 | 16,858 | 160.9 | 323.9 |
| 22 | 32,222 | 15,213 | 17,009 | 161.1 | 342.1 |
| 23 | 32,434 | 15,290 | 17,144 | 161.3 | 362.1 |

RSS climbs with spawned vehicles and levels off near 161 MiB once the queue stops growing quickly. Vehicles that have arrived are still in the table, which is why RSS does not fall overnight.

### Frontend telemetry

The frame magic is `0x54534631`. A full frame for 38,313 edges and 8 bottleneck rows is **459,932 bytes**. Columns are three `f32` arrays. The desktop path measured here is the one in `publish_live`: `write_link_fields`, length-weighted mean speed, top-8 bottleneck rank, `encode_frame`, the publish `clone_from`, and the invoke `clone` inside `frame_since`.

Times are means of 20 iterations after one warmup, taken on the live load at 16 threads:

| Stage | Low | Peak | Gridlock |
| :--- | ---: | ---: | ---: |
| `write_link_fields` | 0.373 ms | 0.553 ms | 0.771 ms |
| Mean speed scan | (inside combined) | (inside combined) | (inside combined) |
| Bottleneck rank | (see combined) | (see combined) | (see combined) |
| `encode_frame` | 0.120 ms | 0.155 ms | 0.146 ms |
| Publish copy | 0.013 ms | 0.023 ms | 0.029 ms |
| Invoke copy (`frame_since`) | 0.013 ms | 0.023 ms | 0.029 ms |
| **Full publish** | **1.04 ms** | **1.76 ms** | **2.27 ms** |

A cold codec bench on a synthetic frame of 459,860 bytes, without walking live queues, encodes in 0.116 ms and copies in 0.011 ms. Rust `decode_frame` is a header parse plus borrowed column slices: 0.06 µs.

The JavaScript unpack is `tools/bench-frame-decode.mjs`, the same `DataView` layout as `ui/src/frame.ts`, run under Node on that synthetic frame (200 iterations):

| JS operation | Mean |
| :--- | ---: |
| Header parse + typed-array views (no column copy) | 1.56 µs |
| Same parse plus three new `Float32Array` copies | 188 µs |

The UI uses views, not copies. Copying the three columns is still under 0.2 ms.

A 60 FPS budget is 16.7 ms and a 30 FPS budget is 33.3 ms. Building and copying the frame costs 1.0–2.3 ms, which is 6–14% of a 60 FPS frame at these loads. **Deck.gl / MapLibre present time and GPU memory were not measured in this run.** The bench page (`tools/bench-gpu.html`, 38,313 paths, 169,599 vertices) was not hosted, so no WebGL timings were taken. Those cells are left empty on purpose.

What the CPU side does show: the IPC blob is 450 KiB, the Rust publish is about 2 ms under the saturated load, and the JavaScript view is negligible next to a frame. The remaining risk for 60 FPS is the layer update in `NetworkMap.paint`, which constructs two new `PathLayer`s over the full path table on every appearance change. That cost was not timed.

## 3. Theoretical Performance Comparison

Comparisons below are architectural. Competitor tick rates were not measured in this session and are not quoted.

### Versus microscopic simulators (SUMO, Aimsun)

Those models integrate car-following, lane changing, and often signal actuation per vehicle. This engine moves packets through a link-transmission / spatial-queue step: one FIFO per directed edge, storage capped at lanes × jam density × length, inflow capped by residual storage. The detail that is absent is lane-level behaviour. The detail that is present is spillback without deleting vehicles.

On this laptop that trade buys a full Thessaloniki graph in a 27 ms cache load and a 161 MiB day. Measured speedups are 466–1,349× at about 3,500 vehicles, about 100–160× at 30,000, and 29–49× at 80,000. A 1,000× run with 50,000 vehicles was not observed. At 30,000 vehicles the best figure is 158×; at 80,000 it is 49×. City-scale microscopic tools typically need a cluster or a mesoscopic mode to sit in that vehicle range. This process does not.

### Versus JVM mesoscopic frameworks (MATSim)

MATSim's mobsim is event-driven and runs on the JVM, so a run pays class loading, JIT warmup, and garbage-collection pauses proportional to event allocation. This process has none of those pauses. It does allocate every tick, up to 52 MiB of short-lived path buffers under spillback, which is allocator traffic rather than a stop-the-world collector.

Startup is the cache decode: 28 ms to a usable graph, 2.5 ms to an empty stepper. The heavy 24-hour scenario peaks at 161 MiB RSS. A JVM city model of the same network usually carries the graph, the population, and the runtime in a heap several times larger, and the first minutes of a run include warmup that this release binary does not. The population here is a boundary rate plus per-vehicle paths, not a pre-built all-day plan list; path bytes (75 MiB after 82k trips) are the population's main cost.

### Versus A/B Street

A/B Street is a Rust microscopic model aimed at interactive edits of one city's lanes and signals. It keeps a vehicle-level representation, so an extreme gridlock is a large set of lane positions and collision checks. This engine's gridlock limit on the same kind of city graph is the spatial queue: 80,000 active vehicles, almost all waiting, in a 122–151 MiB process, stepping at 20–34 ms. Nothing is despawned to clear the jam. The batch number beside that is one congested 24-hour scenario in 6.03 minutes on a laptop, which is the rate a headless study can use. A microscopic Rust model can show a single junction more faithfully; it will not match this step cost once every queued vehicle is a simulated body.

## 4. Hardware System Requirements & Capacity Matrix

Rows are anchored to the i7-1270P measurements. The 24-hour figure in the last column is that same demand: 100,000 requested boundary vehicles, of which 81,639 were spawned, ending in gridlock. Lighter days are cheaper; the night hours of this run cost about a second of wall time each.

The 4-thread and 8-thread day times were not run as full days. They are the measured 362 s scaled by the peak-load tick ratio against the 16-thread result (peak ticks: 7.983 ms at 4 threads, 6.794 ms at 8, 6.348 ms at 16). Departure routing is serial, so the true gap is smaller than that tick ratio. The scaled figures are upper-leaning projections, marked as such.

| Tier | Target workflow | Recommended CPU | Minimum RAM | GPU / display | 24h run time |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Minimum** | One city in the GUI at 1×–20×. Gridlock at 4 threads still steps at 46×, so 20× playback fits. 100× does not fit once ~80,000 vehicles are waiting. | 4-core x86_64 or ARM64 | 4 GB. The simulation itself peaked at 161 MiB; 4 GB is there for the OS and the webview. | Integrated graphics, WebGL 2.0. GPU frame time was not measured. The CPU publish is ~2 ms. | **7.6 min projected** (362 s × 7.983/6.348) |
| **Recommended** | Desktop research at 100× on peak demand (~30,000 active). 8 threads measured 147× on that load. Live A/B diffs add a second publish of the same 450 KiB frame. | 8-core (this class: Ryzen 7, Apple M-series, or a recent Intel H/P part) | 8 GB. 16 GB if the webview, a browser, and a second run are open together. | The same integrated GPU is enough for the CPU-side budget measured here. | **6.5 min projected** (362 s × 6.794/6.348) |
| **HPC / batch** | Headless Monte Carlo. One heavy day on this 16-thread laptop. | 16+ logical processors. Extra threads past 8 did not speed up a single gridlock step on this chip; they help when several runs share the machine. | 32 GB when many runs are resident. One run peaked at 161 MiB, so the RAM is for concurrency, not for the graph. | None. | **6.03 min measured** for one day. A 50-run batch, if 16 runs proceeded at that single-run rate, would be about **24 min**. That is a lower-bound projection: the experiment runner nests Rayon inside Rayon, so runs share the pool and will take longer. |

## 5. Identified Bottlenecks & Optimization Vectors

The functions that dominate are visible in the stage split and the allocation counter.

1. **`decide_next` / `decide_entry` clone `vehicle.path` on every visit.** The window reroutes a few hundred vehicles and still allocates once per active vehicle. Under spillback that is 78,868 allocations and 52.2 MB of churn per tick, and it caps scaling at about 1.6×. Keep the path in place unless a search actually replaces it. A small-vector or arena copy for the rare reroute removes the steady-state traffic.

2. **`NodeProposal` builds six new `Vec`s per busy node, and there are ~500–700 busy nodes per tick.** Recycle those buffers per worker, the same way the Dijkstra scratchpad is already recycled. Combined with (1), a steady tick can approach the preallocated edge arrays that already exist (`discharge_credit`, `experienced`, `occupied`, `busy_ids`).

3. **`note_saturation` walks every waiting id every tick.** It is 4% of a 3,500-vehicle tick and 22% of an 80,000-vehicle tick, and it is serial, after the Rayon join. Maintain the spillback count as vehicles enter and leave a full link, or sample it on a slower cadence than the flow step. `audit` has the same shape: it rechecks every occupied edge every second.

4. **`release` routes departures one Dijkstra at a time on the calling thread.** A search is ~1 ms. The 24-hour run spawned 81,639 vehicles, and that serial search is the reason a day takes minutes rather than seconds. The scratchpad is already thread-local. Farming the departure batch across the pool, with one scratchpad per worker, is the change that shortens Monte Carlo runs. SIMD inside the link loop will not, because apply is under 6% of the tick and the arithmetic is not the wait.

5. **Hot edge fields live in a 224-byte `Edge` next to polylines and names.** Split length, free-flow speed, capacity, storage, and head node into parallel arrays. The discharge loop then streams a few cache lines per occupied edge instead of a 224-byte record. This is a layout change, not a new model.

6. **Queues are `Vec<VecDeque<u64>>`, one heap buffer per edge.** The microbenchmark shows a 3.1× penalty against a contiguous `f64` scan at the real edge count. A slab of queue nodes, or one arena reset per step, keeps the occupied-edge walk on consecutive cache lines. Worth doing after (1) and (2); those dominate the tick today.

7. **Arrived vehicles keep their paths.** 74.7 MiB of the 96 MiB stepper at the end of the day is path storage, including trips that finished. Dropping `path` when `arrive_s` is set does not remove a vehicle from a link. It stops a long study from retaining every historical polyline.

8. **Rayon join plus the 64-node serial cutoff.** Below 64 busy nodes the step stays single-threaded by design. Above that, the measured curve is flat after two threads on this CPU. Wider pools still cost RSS (about 29 MiB from 1 to 16 threads) and, on the peak load, the two-thread run was slower than one thread (74× vs 96×). A fixed pool of two to four workers matches the scaling that was actually measured. More cores pay off across runs, not inside one step, until the per-task allocation is gone.

SIMD vectorization of the link update and a custom arena are both real follow-ons. The arena matters only as the backing store for (1) and (2). Vectorizing the `f64` edge state matters only after the tick stops spending itself in `alloc` and in the waiting-vehicle walk. Cache-line alignment of the SoA arrays in (5) is the right companion to that split; aligning the current 224-byte `Edge` would not change the clone traffic.

## Reproducing

```
cargo run -p sim-cli --release -- benchmark
```

The command writes `docs/perf-results.json`. `--only loads`, `--only scale`, and `--only day` run subsets. `--threads` and `--vehicles` run a single window. The JavaScript unpack is `node tools/bench-frame-decode.mjs <frame.bin>`.
