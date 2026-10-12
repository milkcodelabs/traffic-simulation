# Data architecture

Canonical layout for the traffic simulation core. Later stages (time stepping, dynamic assignment, scenario diffing, the Deck.gl telemetry view) must extend these types, not replace them with parallel structures.

The compiled network is a static, city-agnostic road graph. Simulation time, signal phase, and demand are runtime state. OpenStreetMap is an input, never the in-memory model.

## Workspace

```
crates/sim-core     graph, fundamental diagram, grades, turns, signals, demand profile, link state, cache
crates/sim-engine   discrete link-transmission step, spatial queues, signals, bounded-rationality routes
crates/sim-ingest   OSM XML/PBF, presets, clip, contraction, compilation into sim-core::Network
crates/sim-ipc      command DTOs, the viewport blob, and the per-frame link-state blob
crates/sim-cli      headless ingestion and the paired Monte Carlo experiment
src-tauri           Tauri v2 shell
ui                  React / TypeScript / Tailwind research console
```

`sim-core` does not know about OSM or Tauri. `sim-ingest` produces a `Network`. The desktop shell and the CLI both call the same compile path.

## Identifiers

Dense and index-identical. `NodeId(i)` is `network.nodes[i]`. Same for edges, turns, signals, and zones. The invalid sentinel is `u32::MAX` and is never a live index.

OSM identifiers stay `u64` metadata. They are not graph indices. Synthetic vertices inserted where a polyline crosses the bounding box use ids at and above `0xF000_0000_0000_0000` and `Node.synthetic = true`.

## Geography

- `LonLat` is WGS84 degrees.
- `Xy` is UTM metres in the zone of the bounding-box centroid. Northern hemisphere uses false northing 0; southern uses 10_000_000. False easting is 500_000.
- `BBox` is `{ min_lat, min_lon, max_lat, max_lon }` in degrees, validated so min < max and latitudes stay inside [-90, 90].
- Edge length is the polyline length in UTM metres, never a chord through intermediate vertices and never a degree-space length.
- A directed edge stores both the UTM polyline and the WGS84 polyline. Rendering reads WGS84. Grades and lengths read UTM.

### Thessaloniki preset

`thessaloniki` is the calibration baseline.

| field | value |
| --- | --- |
| min_lat | 40.5700 |
| min_lon | 22.8850 |
| max_lat | 40.6750 |
| max_lon | 23.0150 |
| regime | European urban |
| UTM | zone 34 north |

The box covers the historic centre, Kalamaria, Toumba, Sykies, Evosmos, Ampelokipoi, and the inner ring approaches. A caller can still pass any other box or an OSM file. The preset is data, not a special compile path.

## Static graph

`Network` is the serialized artifact. Format version is `1`. Envelope bytes are `TSIM`, then a little-endian `u32` version, then bincode of `Network`. A version mismatch is a hard error. Caches are not migrated in place.

### Node

| field | meaning |
| --- | --- |
| kind | `Intersection`, `Gateway`, `Roundabout`, `RampSplit`, `Signalized`, `DeadEnd` |
| lonlat, xy | position |
| elevation_m | `Some` only when a sampler returned a height |
| osm_id | source id, or 0 when synthetic and no OSM node exists |
| synthetic | bounding-box clip vertex |
| signal | `Some(SignalId)` when a controller is attached |

Kind rules, in order: signal tag wins `Signalized`; a `mini_roundabout`, a roundabout self-loop, or a node on a `junction=roundabout` carriageway with degree greater than 2 is `Roundabout`; a clip vertex or a degree-1 node within 40 m of the box boundary is `Gateway`; a degree-1 node elsewhere is `DeadEnd`; a node whose incident classes mix a `*_link` with a non-link is `RampSplit`; everything else is `Intersection`.

### Directed edge

One OSM way that is bidirectional becomes two edges. Geometry of the reverse edge is the reversed polyline. Grade sign flips. `lanes:forward` and `lanes:backward` apply per direction.

| field | meaning |
| --- | --- |
| from, to | endpoints; a roundabout with one remaining vertex may be a self-loop |
| class | normalized OSM highway class, including `*_link` and `Service` |
| lanes | lanes in the direction of travel, at least 1 |
| length_m | polyline length |
| grade_rad | length-weighted mean of segment grades, positive uphill |
| uphill_grade_rad | length-weighted mean of `max(grade, 0)` |
| free_flow_speed_mps | posted or inferred speed, after the uphill speed adjustment |
| base_free_flow_speed_mps | speed before the grade adjustment |
| critical_speed_mps | diagram parameter; equals free-flow speed on the triangular diagram |
| capacity_veh_per_s | link saturation flow after lanes and the uphill penalty |
| base_capacity_veh_per_s | link saturation flow before the grade penalty |
| jam_density_per_lane | vehicles per metre per lane |
| storage_max_veh | `lanes * jam_density_per_lane * length_m` |
| osm_way_ids | source ways in travel order. Restrictions match the last id on an incoming edge and the first id on an outgoing edge, which are the ways that touch the via node after contraction |
| name, road_ref | source labels, kept from the first way in the chain |
| flags | roundabout, bridge, tunnel, destination-only access, dead-end reversal |

Edges shorter than 1 m are dropped. They are geometric duplicates, not vehicles.

### Adjacency

Compressed sparse row, both directions:

- `out_offset[node] .. out_offset[node + 1]` indexes `out_edges`
- `in_offset` / `in_edges` likewise

Offsets are `u32` and length `node_count + 1`. This is the layout routing will walk.

### Turns

Built at compile time for every ordered pair `(incoming edge, outgoing edge)` at a node, except a pair that would depart along the same directed edge it arrived on when another exit exists.

`Movement` uses navigation bearings (0 = north, clockwise) and right-hand traffic:

| delta = outgoing bearing − incoming bearing, wrapped to (−π, π] | movement |
| --- | --- |
| absolute delta ≤ 35° | through |
| 35° < delta < 150° | right |
| −150° < delta < −35° | left |
| absolute delta ≥ 150° | u-turn |

U-turns are disallowed by default. They are allowed only at a non-gateway dead end, so a vehicle can reverse without leaving the network. Gateways are sources and sinks: the vehicle exits the model there by completing its trip, which is the boundary condition, not a deletion inside a link.

A non-gateway node whose OSM ways arrive one-way and leave no exit gets a synthesized reverse edge per incoming edge (`EdgeFlags.dead_end_reversal`). The reverse copies length, class, lanes, and speed, and flips grade. It is a traversal back along the same carriageway, not a jump to another link.

`Turn.allowed` starts from that geometric default. OSM `type=restriction` relations then edit it.

- `no_left_turn`, `no_right_turn`, `no_straight_on`, and `no_u_turn` forbid from-way → to-way pairs at the via node whose movement matches the restriction. `no_entry` forbids every pair onto that way.
- `only_*` keeps pairs onto the to-way that match the named movement and forbids every other exit from the same incoming edge. If no pair matches the movement, every pair onto the to-way is kept.
- A via-way restriction applies when the shared node lies on the via way.
- When the destination way is bidirectional, only edges that leave the via node are candidates.

Turns are stored in node order with a CSR (`turn_offset`) so a node’s movements are one contiguous slice.

## Fundamental diagram

Default family is the triangular Newell / Daganzo diagram, because the link-transmission step consumes it directly. Greenshields and Smulders are evaluators on the same edge parameters. `critical_speed = free_flow_speed` reduces Smulders to triangular, following the link-transmission reading of Smulders (1990): free-flow branch linear in the speed–density plane, congested branch linear in the flow–density plane.

Density `k` below is vehicles per metre on the whole link. Jam density on the link is `jam_density_per_lane * lanes`.

Triangular, with backward wave speed `w`:

```
q(k) = v * k                         if k <= k_c
q(k) = w * (k_jam - k)               if k_c < k < k_jam
q(k) = 0                             if k >= k_jam
k_c  = k_jam * w / (v + w)
q_max = v * k_c
```

Greenshields (1935):

```
u(k) = v * (1 - k / k_jam)
q(k) = k * u(k)
k_c  = k_jam / 2
q_max = v * k_jam / 4
```

Smulders, with critical speed `u_c` and capacity `q_c` (`k_c = q_c / u_c`):

```
u(k) = v - (v - u_c) * k / k_c       if k <= k_c
q(k) = q_c * (k_jam - k) / (k_jam - k_c)    if k_c < k < k_jam
q(k) = 0                             if k >= k_jam
```

Class defaults below fix `v`, `w`, and `k_jam`. Capacity is derived. It is never typed in as an independent guess. Parameter construction returns an error if `v <= 0`, `w <= 0`, `k_jam <= 0`, `lanes < 1`, or, for Smulders, `k_c >= k_jam`.

Per-lane jam spacing and wave speed:

| class group | jam spacing | k_jam per lane | wave speed |
| --- | --- | --- | --- |
| motorway, trunk, and their links | 8.0 m | 0.125 veh/m | 5.56 m/s |
| all other included classes | 7.5 m | 0.133333 veh/m | 5.00 m/s |

## European urban speed and lane fallback

Used only when the OSM tags are absent or unparsable. A parsed `maxspeed`, `lanes`, `lanes:forward`, or `lanes:backward` always wins.

| class | speed (km/h) | lanes in the travel direction |
| --- | --- | --- |
| motorway | 130 | 2 |
| motorway_link | 70 | 1 |
| trunk | 90 | 2 |
| trunk_link | 50 | 1 |
| primary | 50 | 2 if one-way, else 1 |
| primary_link | 40 | 1 |
| secondary | 50 | 1 |
| secondary_link | 40 | 1 |
| tertiary | 40 | 1 |
| tertiary_link | 30 | 1 |
| unclassified | 40 | 1 |
| residential | 30 | 1 |
| living_street | 20 | 1 |
| service | 20 | 1 |

`maxspeed` accepts plain numbers, `km/h`, `mph`, and the Greek conventional values `GR:urban` (50), `GR:rural` (90), `GR:trunk` (110), `GR:motorway` (130). `signals` and `none` fall back to the class default (`none` on a motorway is 130, the national limit, not an unbounded speed).

When `lanes` is the only lane tag on a bidirectional way, each direction receives `floor(lanes / 2)`, and the leftover lane goes to the forward direction. Each direction still receives at least one lane. The forward direction is the OSM way direction.

Implicit one-way: `motorway`, `motorway_link`, and `junction=roundabout` or `junction=circular`, unless `oneway=no`. `oneway=-1` reverses the vertex order.

## Grade and uphill capacity

`ElevationSampler` is the only elevation interface:

```
fn sample(lon_deg, lat_deg) -> Option<f64>
```

`FlatElevation` returns one constant height, so grade is zero. That is a real flat-terrain case. `GridElevation` is a row-major raster with bilinear samples and `NaN` for missing cells. Bridge and tunnel edges do not sample a ground raster; their grade stays 0 because a ground DEM is not the deck grade.

Segment grade is `atan2(Δh, horizontal_length)`. The edge stores the length-weighted mean and the length-weighted uphill component.

Uphill adjustment, with grade fraction `G = tan(θ_uphill)` and heavy-vehicle share `h` (default 0.08):

```
v     = v0 / (1 + 0.5 * G)
α     = 1.5 * (1 - h) + 6.0 * h
q     = q0 / (1 + α * G)
```

Downhill grades do not increase saturation flow. Both base and adjusted values are stored. Jam density is unchanged: grade does not create or remove storage.

The triangular operating diagram is rebuilt from the adjusted free-flow speed and the penalized capacity. Greenshields keeps its textbook shape at the adjusted free-flow speed, then scales flow so the peak equals the penalized capacity. Smulders takes the penalized capacity as `q_c` and sets critical speed to `ratio * v`.

## Storage and spillback

`Inflow_e(t)` in the project specification is the vehicle increment admitted to the link, not a rate. Residual storage is

```
available = max(0, storage_max_veh - vehicles_on_link)
```

and that quantity is the hard ceiling on the increment. The rate form used later by the time stepper is `available / Δt`. Discharge capacity is a separate bound. This layer does not divide by time.

Link transmission state, one row per edge:

| array | meaning |
| --- | --- |
| cumulative_in | vehicles that have entered since the run started |
| cumulative_out | vehicles that have left |
| cumulative_demand | vehicles whose path has assigned them to this link and that are waiting upstream when the link is blocked |

`vehicles = cumulative_in - cumulative_out`. A residual inside `1e-8` vehicles is treated as zero in the density view and reported separately by `conservation_residual`. Anything more negative is a conservation failure and is not clamped into existence or out of it. Initial state is all zeros, so every link starts empty and `available == storage_max`.

There is no function here that deletes vehicles to clear a jam.

## Signals

A controller is attached where a surviving node is tagged `highway=traffic_signals` or `crossing=traffic_signals`.

The plan is derived from the approaches that actually exist:

1. Cluster incoming edges into opposing groups by bearing.
2. Use two phases when one approach class strictly outranks the cross street; otherwise one phase per occupied quadrant group, capped at four.
3. Green shares follow incoming lane sums, with a minimum green of 8 s.
4. Yellow is 3 s and all-red clearance is 1.5 s on every phase.
5. The cycle is scaled toward 90 s when a primary-or-higher approach is present, otherwise toward 60 s.
6. Offset is 0 until coordination exists.
7. The night window 00:00–06:00 local time is stored on the controller. Inside it the scheduled mode is flashing amber; outside it the mode is the fixed cycle. `tau_yield_s` is 4.5 s and is the gap-acceptance delay the flashing-amber and unsignalized yield rule will apply to the minor movement.

`SignalMode` is `FixedCycle`, `FlashingAmber`, or `Off`. Changing mode is a change of controller state. It is not a change of vehicle position.

Unsignalized nodes keep no controller. Priority is the edge class rank. The yield rule will read that rank later.

## Demand profile and zones

Zones are spatial buckets, not invented trip tables.

- Each gateway node is its own `ZoneKind::Gateway`.
- Remaining nodes fall into 1 km activity cells. Empty cells are omitted.
- No production or attraction totals are filled in at compile time.

`DiurnalProfile` is 24 hourly fractions that sum to 1. The shape is a normalized mixture: night floor weight 0.08, morning Gaussian at 08:00 (σ = 1.2 h, weight 0.28), midday Gaussian at 13:00 (σ = 2.5 h, weight 0.34), evening Gaussian at 17:30 (σ = 1.4 h, weight 0.30). Origin–destination volumes are a later input. They must not be synthesized as a placeholder matrix.

`CommodityId` will name a destination zone once assignment exists. The field is reserved on the scenario demand scaler, not populated with fake paths.

Bounded-rationality inertia is a property of the future router: a path is replaced only when its experienced travel time exceeds `inertia * baseline_travel_time`, with inertia in [1.3, 1.5]. The constant `DEFAULT_ROUTE_INERTIA = 1.4` lives with the demand types so the threshold is not reintroduced ad hoc.

## Scenario overlay

A `Scenario` does not mutate the compiled geometry. It stores:

- signal schedule override: unchanged, force fixed cycle, or force flashing amber
- optional replacement night window. When it is set, that window replaces each controller’s own night window for mode selection and for idle-step boundaries
- `global_speed_scale` and `global_demand_scale`, both defaulting to 1
- per-edge capacity scales (0 closes the link)
- per-edge speed scales
- per-zone demand scales

`scaled_capacity` and `scaled_speed` multiply the edge’s adjusted base values by every matching scale, then clamp at zero. Speed is also multiplied by `global_speed_scale`. Missing per-edge entries scale by 1. `scale_factors` builds the same product once per edge for the routing and receiving passes. A capacity scale of 0 closes the link: nothing enters and nothing leaves. Vehicles already aiming at it stay on the upstream link.

Closing lanes on the ring or motorway writes a capacity scale of `(lanes − closed) / lanes` onto edges whose class is motorway, motorway link, trunk, or trunk link, or whose name or ref contains `ring` or `περιφερ`. The polyline is unchanged. The desktop clamps a global speed scale to [0.2, 1.5], a demand multiplier to [0, 5], and a lane closure to at most 4.

## Time stepper

`sim-engine::Simulation` advances a compiled `Network`. It does not allocate a second graph. Link counts stay in `LinkTransmissionState`: entering adds one to `cumulative_in`, leaving adds one to `cumulative_out`, and the FIFO on that edge has the same length. `cumulative_demand[e]` is the current stock of vehicles that are ready to enter `e` and are still held on the upstream link because `e` is full or closed. It is not a vehicle-step accumulator.

Trips are an explicit list `{depart_s, origin, destination, inertia}`. The compiler still does not invent an origin-destination matrix. A spawned trip is active until it leaves the network at its destination node. The census identity is `spawned = active + completed`, with active equal to vehicles on links plus vehicles waiting at their origin. Completing at a gateway is the sink boundary. No step deletes a vehicle to clear a queue.

One step of length `Δt` is simultaneous, using the vehicle counts at the start of the step:

- A vehicle may leave its current link only after `enter_time + length / v` (minor approaches under priority also wait `tau_yield`).
- Vehicles leave in FIFO order. The number that may leave an open approach is the integer part of a fractional capacity carry, so the discharged count tracks `q Δt` without creating a vehicle to fill a fraction.
- Red, all-red, and a capacity of 0 offer no discharge. Unused green is not banked into a later step.
- Inflow into `e` cannot exceed `max(0, S_max,e − N_e)`. A full link accepts nothing even if it also discharges in the same step, which is the project spillback bound. The rejected vehicle remains on the upstream link and occupies its storage, so the queue moves backward when that link fills.
- A vehicle moves at most onto the next link of its path. It does not skip a link.

Fixed-cycle discharge is green and yellow for the approaches named on the active phase. Flashing amber and unsignalised nodes use class rank: approaches that share the highest incoming rank keep priority; every lower rank waits `tau_yield_s` and yields the step when a priority vehicle is already at the stop line. `Scenario::forced_mode` overrides the clock. The clock origin defaults to 12:00, so an unchanged controller is on its fixed cycle; the night window still selects flashing amber. The desktop session sets the origin to midnight so the scrubber and the signal clock are the same day.

Each step adds `active × Δt` to vehicle-hours (VHT). A vehicle that leaves a link adds that link’s length to vehicle-kilometres (VKT) and `max(0, time_on_link − length / v)` to cumulative delay, and the same three quantities to that edge’s accumulators. Mean speed on an edge is distance divided by travel time once any vehicle has left it; otherwise the observation is unobserved. The drawing sample for an edge is `v / v_free`: 1 when the edge is empty, otherwise free-flow time divided by `max(length / v, N / q)`, clamped to `[0, 1]`. Density is `N / S_max`. The queue count is the number of vehicles that have already spent their free-flow time and are still waiting to discharge. `Simulation::snapshot` copies that mutable state so the desktop thread can rewind without cloning the graph. A step with no due trip and no vehicle still in the network only advances the clock and the per-edge discharge remainder. Several such seconds collapse into one remainder update: an open approach adds `capacity × open seconds` and keeps the fractional part, and a seek splits that update where the night window changes the signal mode. The move, routing, and audit passes run once a trip is due.

Habitual routes are Dijkstra on free-flow time `length / v`, expanding only turns with `Turn.allowed`. At a decision (origin entry, or the downstream node of a departure) the driver compares the experienced cost of the remaining habitual path with `inertia *` its free-flow cost. Experienced cost of a link is `max(length / v, N / q)`, or infinity when `q = 0`. The default inertia is 1.4. A path is replaced only when that ratio is exceeded, and only by another finite path. Distinct shortest-path queries in a step run on a Rayon pool. Proposals for every node are also computed on that pool from the shared start-of-step snapshot; the merge applies every departure before every entry so a vehicle that just entered cannot be popped as if it had already queued.

## OSM compilation

Supported inputs:

1. Preset name (`thessaloniki`).
2. Explicit bounding box, downloaded from Overpass.
3. Local `.osm.xml` / `.osm` or `.osm.pbf`.

Overpass endpoints, in order: `https://lz4.overpass-api.de/api/interpreter`, `https://overpass-api.de/api/interpreter`, `https://overpass.kumi.systems/api/interpreter`, `https://overpass.private.coffee/api/interpreter`, then `https://overpass.openstreetmap.fr/api/interpreter`. The client keeps the first response that is a complete `<osm>…</osm>` document. Each included highway class is requested separately; a single regular expression is more likely to time out. The raw response is written under `data/raw/` and reused until the caller passes refresh. The compiled cache is written under `data/cache/` with extension `.simnet`.

Degree-2 contraction must leave the summed UTM polyline length unchanged to within 0.1 mm. `CompileOutput` records the length before and after that step. Dead-end reversals are added afterwards and are not part of that comparison. `Network::weak_components` reports undirected connectivity; one-way edges still join their two endpoints.

Included highway values: motorway, trunk, primary, secondary, tertiary, unclassified, residential, living_street, service, and the four `*_link` classes. Dropped before the graph exists: `access` or `motor_vehicle` or `motorcar` in `{no, private}`, `highway=proposed|construction`, `area=yes`, and `service` in `{parking_aisle, parking, driveway, emergency_access, slipway}`. `access=destination` is kept and flagged.

Pipeline:

1. Parse nodes, ways, and restriction relations. A PBF or XML extract is a city extract or a bbox download. The reader retains node coordinates for the file; it is not a planet parser.
2. Clip polylines to the bbox. Crossings become synthetic gateway vertices. Ways that never enter the box disappear.
3. Mark graph vertices: way endpoints, nodes used by two or more way visits, signal / motorway junction / mini-roundabout tags, and synthetic clip vertices. Other shape points stay polyline vertices.
4. Cut ways at graph vertices and emit one directed edge per travel direction.
5. Contract a vertex only when its directed in-degree and out-degree are both 1, it is not signalized, not a mini-roundabout, not a motorway junction, and the upstream and downstream edges agree on class, lane count, speed (within 0.1 m/s), and bridge/tunnel/roundabout flags. Geometry is concatenated and the joint vertex is not duplicated. Lengths add. A pure cycle is contracted along arcs between non-contractable vertices; a single-vertex ring remains a self-loop with the circular polyline.
6. Project, sample elevation, compute grades, apply the fundamental diagram, build both CSRs.
7. Build turns, apply restrictions, attach signal plans, classify nodes, assign zones.
8. Refuse to cache an empty network.

Warnings are counts (dropped ways, dropped short edges, unreadable speeds), not per-feature log lines.

## IPC

JSON commands, camelCase fields:

| command | returns |
| --- | --- |
| `list_presets` | preset id, label, bbox |
| `ingest_network` | cache path, raw path, `NetworkStats`, warning strings |
| `network_summary` | same report read back from a cache |
| `network_geometry` | binary viewport blob as an array buffer |
| `compiled_cache` | path of `data/cache/thessaloniki.simnet` when that file exists |
| `open_simulation` | loads a cache onto the background simulation thread |
| `simulation_control` | pause, speed multiplier, boundary veh/h, signal schedule, seek second |
| `simulation_scenario` | global speed scale, demand multiplier, ring/motorway lanes closed |
| `simulation_capture_baseline` | stores the current per-edge delay and mean speed as run A |
| `simulation_restart` | rebuilds the run from midnight under the current scenario, keeping run A |
| `simulation_diff` | binary comparison of run A with the live run |
| `simulation_frame` | the latest frame blob, or empty when `since` is already current |
| `simulation_status` | whether a session is publishing, and the last step error |

`NetworkStats` carries node, edge, turn, banned-turn, signal, gateway, and zone counts, total length and lane-kilometres, mean edge length, contraction count, bbox, and diagram name. The simulation thread is the only caller of `Simulation::step`. The webview polls `simulation_frame` from a timer and paints the previous buffer if no new one has arrived.

Viewport blob, little-endian:

```
magic u32 = 0x54534E31
edge_count u32
repeated edge_count times:
  edge_index u32
  class u8
  lanes u8
  speed_kph u16
  point_count u16
  point_count × (lon f32, lat f32)
```

`f32` degrees are for drawing. Simulation state never travels in this blob.

Frame blob, little-endian, magic `TSF1`:

```
magic u32 = 0x54534631
version u32 = 1
sequence u64
time_s f64
speed f32
flags u32          bit 0 paused, bit 1 seeking, bit 2 unchanged
mean_speed_mps f32
vht_s f64
vkt_m f64
delay_s f64
spawned u32
active u32
completed u32
edge_count u32
bottleneck_count u32
bottleneck_count × (edge u32, speed_ratio f32, queue_veh f32)
edge_count × speed_ratio f32
edge_count × density_ratio f32
edge_count × queue_veh f32
```

Comparison blob, little-endian, magic `TSD1` (`0x54534431`), 36-byte header:

```
magic u32
edge_count u32
improved u32
degraded u32
neutral u32
baseline_time_s f64    NaN when no baseline has been captured
compared_time_s f64
edge_count × class u8  0 neutral, 1 improved, 2 degraded
```

A link is neutral when both runs are unobserved and delay is about zero, or when the speed change stays inside 5% of free-flow (free-flow at least 1 m/s) and the delay change stays inside 1 second. The score is `Δspeed / v_free − Δdelay / 30`. Positive is improved (blue), negative is degraded (red), otherwise gray. Unobserved speed is treated as free-flow and unobserved delay as 0. The desktop refreshes this packet about four times a second while a baseline exists. The map replaces the congestion colours with these classes while A/B diff is on.

The frame header is 80 bytes. Edge order is the dense edge index, the same order as the viewport blob. A bottleneck row is present only when `queue_veh > 0`, at most eight, highest queue first. When link contents are unchanged, the publisher sets bit 2 and writes `edge_count = 0` with no columns. The webview keeps the previous speed, density, and queue buffers and still advances the clock from the header. A full frame is written on the first publish, whenever a vehicle enters or leaves a link, when a seek finishes, and on each stored visual sample. Boundary demand is a veh/h rate on the compiled gateway nodes. Origins are entry gateways in the giant connected component that can reach an exit gateway under the turn-aware free-flow search. Destinations are exit gateways in that component. A sampled pair with no path is rejected and logged during demand generation and is not spawned. An exit-only cordon node is never used as an origin. Zero emits nothing; it is not an origin-destination matrix. The desktop clock starts at midnight.

## Headless paired experiment

`sim-cli experiment --headless` loads a compiled cache (default `data/cache/thessaloniki.simnet`) and runs two arms on a Rayon pool:

- `baseline-24h-signals`: signal schedule forced to the fixed cycle for the whole horizon
- `night-flashing`: schedule unchanged, night window 00:00–06:00 flashing amber

Demand is the diurnal boundary instrument. Vehicles per hour in hour `h` are `daily_vehicles × global_demand_scale × hour_fraction[h]`, so a full day sums to the daily total. The default is 6 000 boundary vehicles over 86 400 seconds, counted only among pairs the router can path. Origins are entry gateways in the giant component and destinations are exit gateways in that component. A link shorter than one jam spacing has `storage_max_veh < 1` and cannot accept a whole vehicle, so its routing weight is infinite. Trips are not sent onto those connectors. A sampled pair that still has no path is rejected and logged instead of being spawned. This is not an origin-destination matrix. A seed repeats the same headways and destination pairing; the headway multiplier is `0.75 + 0.5 U`. Seeds are `config.seed + (arm << 32) + run`.

The summary CSV has one row per arm and metric (`vht_h`, `vkt_km`, `delay_h`) with `n`, mean, unbiased variance, and a two-sided 95% Student-t interval. Degrees of freedom above 30 use 1.96; ten runs use 2.262. `runs.csv` is one row per seed. `links.csv` and `diff.bin` compare mean delay and mean speed per edge across the equal horizons.

## Invariants the tests lock

- UTM round-trip at Thessaloniki is within 1 m, and a northbound segment’s UTM length agrees with the WGS84 meridional arc within 0.1%.
- Triangular, Greenshields, and Smulders diagrams return 0 at empty and at jam, peak at the critical density, and match the closed-form capacities above.
- Storage equals `lanes * k_jam * length`. Residual storage is the spillback ceiling. The initial link state is empty and conserved.
- The diurnal fractions sum to 1 and both peaks exceed the night hours.
- On the fixture network: a two-degree chain contracts without changing length; a signalized crossroad remains; a one-way primary is one direction; a bidirectional secondary is two; a roundabout with two exits stays a directed cycle of at least two edges; a missing speed uses the European urban default; `lanes:forward` is honoured; a restriction removes the banned movement; a one-way dead end gains a reversal edge; a polyline that crosses the box gains a synthetic gateway; cache bytes load back to the same counts.
- Spillback: a closed downstream link stays empty, the next link stops at `S_max`, and the surplus occupies the link behind it. Opening the closure lets every spawned vehicle complete.
- A 4 000-vehicle wave satisfies `spawned = active + completed` on every step, and the last arrival matches free-flow time plus discharge at the triangular capacity.
- Flashing amber on an otherwise empty major approach cuts mean delay by more than the red interval of the fixed cycle. A driver whose experienced cost exceeds 1.4 times free-flow leaves the habitual path; a driver inside that band does not.
- A queued link reports a speed ratio below free-flow, VHT and VKT accumulate with the vehicles that actually travelled, and restoring a snapshot returns the same clock and the same queue.
