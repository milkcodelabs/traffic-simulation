# Scientific Audit: Mesoscopic Traffic Engine

**Date:** 12 October 2026
**Subject:** `sim-engine` link-transmission / spatial-queue stepper and the compiled Thessaloniki network
**Method:** `cargo test --test scientific_audit`, plus `cargo test -p sim-engine --lib a_handful_of_routable_trips_leave_the_network`
**Harness:** `crates/sim-engine/tests/scientific_audit.rs`

## Executive evaluation

**Verdict: CONDITIONALLY VALID**

The engine conserves vehicles, keeps density inside jam, keeps speed inside free-flow, discharges a queue at its capacity, spills a blocked link upstream without oscillation or loss, clears a 250% overload once demand stops, and repeats a run bit for bit, including a 48-trip sample on the compiled Thessaloniki graph (16,937 nodes, 38,313 edges, 2,009 signals).

It is a single-reservoir spatial queue with a triangular-diagram capacity. It is not a Godunov / cell-transmission discretization of the Lighthill–Whitham–Richards model. On the congested branch the queue sending flow and the triangular fundamental diagram differ by an RMSE of **0.341 veh/s** (capacity in that comparison is 0.563 veh/s). A gateway can also load an empty link up to its entire residual storage in one 1 s step: **40 vehicles** entered a link whose capacity is **0.444 veh/s**. Those two facts are the reason this is not a research-grade kinematic-wave certification.

Two routing defects found during the audit were fixed and the suite was re-run with zero failures. See Remediation.

## Audit scorecard

| Criterion | Rating | What was measured |
|---|---|---|
| Conservation of mass and vehicle-flow physics | Warning | Mass residual is 0 in every checked step. The congested fundamental diagram is not the discharge law. |
| Discretization and numerical stability | Warning | No negative density, no density above jam, no speed above free-flow. Sub-step links are held to the next step boundary. |
| Spillback and shockwave dynamics | Warning | A full link accepts nothing further and does not oscillate. The backward wave is not propagated at \(w\). |
| Dynamic assignment and routing stability | Pass | Both corridors used; 1 majority flip; Theil \(U = 0.061\). A driver with inertia 1000 stayed on the habitual path. |
| Intersection and signal delay | Pass | Minimum trip delay within \(10^{-4}\) s of zero. Occupancy jump across the 06:00 mode boundary was 0. |
| Bit-level reproducibility | Pass | Identical seeds match to the bit, including VHT and a Thessaloniki sample. GEH between repeats is 0. |
| High-congestion stress | Pass | 250% of bottleneck capacity for 600 s cleared 1,005 s after demand stopped. A crossing overload cleared as well. |

Warning means the stated invariant set holds and a named theoretical approximation remains. Fail would mean mass loss, a sign violation, a panic, a deadlock with an open sink, or a non-repeatable trajectory. None of those occurred after the fixes below.

## Numerical evidence

All synthetic cases use \(\Delta t = 1\) s. Residential links in the fixtures are one lane at 10 m/s (triangular capacity 0.444444 veh/s, jam spacing 7.5 m). The Thessaloniki figures use the compiled cache `data/cache/thessaloniki.simnet`.

### Mass and the entrance burst

Eighty vehicles released together onto a 300 m / 150 m / 300 m corridor:

- spawned 80, completed 80, active 0, rejected 0
- mass residual 0 at every step
- first-step entrance occupancy 40, equal to storage 40.0000
- that burst is 90.0 times the one-second capacity
- peak density \(N / S_{max} = 1.0000\)
- cumulative delay 4,130 s, all trip delays \(\ge -10^{-4}\) s

The storage ceiling \(N \le S_{max}\) is enforced. The receiving flow is that ceiling per step, so a cordon can fill residual storage in a single step. Internal nodes do not mint or delete vehicles: completion happens only when the occupied link’s head node is the trip destination, and the census identity `spawned = active + completed` is checked after every step.

Unroutable origin–destination pairs are refused before spawn. On Thessaloniki the boundary filter drops 70 entry gateways and 68 exit gateways that sit outside the routable giant component (16,618 of 16,937 nodes). Refused trips are not part of `spawned`.

### Discretization

An 8.00 m motorway link at 30 m/s has Courant number \(v_f \Delta t / L = 3.75\) and storage of exactly 1.0000 vehicle.

- twelve vehicles all completed
- maximum occupancy 1
- free-flow time 0.2667 s
- shortest recorded travel time 1.0000 s
- fastest implied speed 8.00 m/s, below 30 m/s

The vehicle cannot leave in the same step it enters, and the departure stamp is then clamped to the start of the next step. A link shorter than \(v_f \Delta t\) therefore records at least one second even though the free-flow time is a fraction of a second. Density and speed stay inside bounds. The truncation error is a delay of up to \(\Delta t\), not an overshoot.

On Thessaloniki, minimum link length is 1.06 m, the maximum Courant number is 11.330, and 3,278 of 38,313 links (8.6%) have \(v_f \Delta t > L\).

### Spillback

A downstream link held at capacity 0. Fifty vehicles:

- the 150 m feeder reached its integer storage of 20 at \(t = 73\) s and stayed there
- occupancy decreases while the exit was closed: 0
- the closed link’s occupancy stayed 0
- 30 vehicles stood on the upstream link
- active 50, completed 0 during the closure
- after the closure was lifted, all 50 completed
- peak density 1.0000

The kinematic-wave time for that feeder is \(L / w = 30.00\) s. The stepper does not move a shock along the link. The link is one reservoir: inflow stops when \(N = S_{max}\), while outflow at an open downstream end continues at capacity. That is a consistent single-cell model. It is a coarser object than a backward wave at \(w\).

### Queue discipline and discharge

Sixty vehicles on an 800 m link, headway 0.5 s:

- arrival order breaks: 0
- vehicles completing between \(t = 100\) s and \(t = 200\) s: 44
- capacity integral over that window: 44.44
- absolute error 0.44 vehicles (about 1%)

Within a link the storage is a `VecDeque` popped from the front, and a blocked head stops the vehicles behind it. Across a merge, approaches are drained in edge-id order, each with its own discharge slots, and a single downstream space is given to the earlier edge id in that step. Intra-link FIFO held in the test. Arrival-time priority at an unsignalized merge is not what the node implements.

### Fundamental diagram

The triangular evaluator itself is consistent. With \(v_f = 15\) m/s, wave speed 5 m/s and jam density 0.15 veh/m:

- capacity and the sampled peak are both 0.562500 veh/s
- flow at \(k = 0\) and at \(k = k_{jam}\) is 0
- flow on \(k \in [0, k_{jam}]\) stays inside \([0, q_c]\)

The stepper’s sending flow, once a vehicle has finished free-flow, is \(\min(q_c, v_f k)\) and therefore stays at \(q_c\) over the whole congested branch. Against the triangle, on 15 congested samples, RMSE is **0.340955 veh/s**. At jam density the diagram sends nothing; the queue still discharges at \(q_c\) if the downstream link has room. Jam density in this engine stops inflow. It does not stop outflow.

### Dynamic assignment

A diverge with a short branch scaled to 25% capacity and a longer open branch, demand at 1.6 times the unscaled link capacity, inertia 1.4 except one pinned driver at 1000:

- 113 vehicles finished on the short branch, 307 on the long branch
- the pinned driver stayed on the short branch
- majority-path flips of the on-link share, sampled every 30 s: 1
- Theil inequality coefficient of that share against its own lag-1 persistence forecast: **0.0607** (37 samples)

The assignment equilibrates in the bounded-rationality sense: a driver reroutes when experienced remaining time exceeds `inertia ×` the free-flow time of the current continuation, and not for a smaller gap. It does not solve a strict Wardrop equilibrium, and it does not use the method of successive averages. One flip and \(U = 0.06\) is a stable split, not a corridor oscillation.

Seed noise around that mechanism is smooth. Five seeds, same corridor, metric = VHT per spawned trip:

- below capacity (0.25 \(q\)): coefficient of variation **0.0106**, mean 84.42 s
- above capacity (1.8 \(q\)): coefficient of variation **0.0068**, max/min **1.019**, mean 97.42 s
- repeating seed 7: difference 0

### Signals

Major-road mean trip delay, 20 vehicles:

- fixed cycle: **7.8500 s**
- forced flashing amber: **0.0000 s** within the printed resolution (the value is above \(-10^{-4}\) s)
- gap: 7.85 s

Crossing the scheduled 06:00 change from flashing amber to fixed cycle, with vehicles already on both the major and minor approaches:

- largest occupancy change on any approach in the boundary step: **0**
- largest occupancy change in the whole run: 1, under a per-step discharge of about 0.98 veh/s
- minimum trip delay: within \(10^{-4}\) s of zero
- every trip completed

Flashing amber charges the minor approach a constant yield delay \(\tau = 4.5\) s and suppresses it while a major vehicle is ready. That is a fixed gap, not a sampled headway distribution. The mode switch does not move vehicles by more than one discharge step, and in this run it moved none.

### Determinism

Two independent executions:

- a 25-vehicle corridor: census, VHT bits, VKT bits, delay bits, arrival bits, and paths match
- 70 independent links, 3 vehicles each, enough busy nodes to take the parallel proposal path (`nodes >= 64`): VHT bits match, arrival bits match, GEH on link loads is **0.0**
- 48 Thessaloniki boundary trips for 240 s, executed twice: VHT bits match, arrival bits match, GEH on the first 256 link loads is **0.0**

### Stress and severance

Linear corridor at 2.5 times the middle-link capacity for 600 s (667 vehicles), then a zero-demand window of 7,200 s:

- peak density 0.9937
- network empty at \(t = 1{,}605\) s
- recovery time after demand stopped: **1,005 s**
- rejected trips: 0
- mass residual 0, and every trip delay within tolerance

Two approaches crossing at one intersection, each at 2.5 times exit capacity for 300 s (668 vehicles):

- empty at \(t = 831\) s
- peak density 0.9937

A closure shorter than the 300 m reroute gate (120 m), with a longer alternate:

- 8 vehicles, all diverted onto the open branch
- occupancy of the closed link remained 0
- a negative capacity scale is clamped to zero, does not panic, and holds 5 vehicles off the only open path until the link is reopened; those 5 then complete

Thessaloniki, longest edge forced to capacity 0, 12 boundary trips, 60 s: the step returns normally and the mass residual stays 0.

A separate engine test, re-run for this audit, sent 8 Thessaloniki boundary trips and advanced 7,200 s: **8 of 8 completed**, VHT 1.05 h, VKT 42.09 km, mean speed 40.0 km/h. The engine’s own conservation check accepted that run.

The new 48-trip Thessaloniki sample was stopped at 240 s with 12 completed and 36 still traveling. Residual 0. That sample is a conservation and repeatability check, not a 24-hour demand forecast.

## Theoretical limitations

These are properties of the model that the tests measured or that the code implements. They were not “fixed,” because changing them would replace the model.

1. **Congested branch.** Outflow after free-flow is capacity until the queue is gone. The triangular, Greenshields, and Smulders evaluators are used to build that capacity at compile time. They are not re-evaluated as \(q(k)\) inside the step.
2. **Receiving flow.** Inflow in one step may be as large as residual storage. The Newell receiving function \(\min(q_c, w(k_{jam}-k))\) is not applied at the entrance.
3. **One cell per link.** Spillback across a link has no internal travel time. The upstream end is blocked in the same step the reservoir fills.
4. **Short links.** Any link with \(v_f \Delta t > L\) pays a pipeline delay of up to one step. On the 8 m fixture that turned 0.267 s of free-flow into 1.000 s. About 8.6% of Thessaloniki links are in that regime. The compiled graph also contains links down to 1.06 m.
5. **Vehicle-hours.** VHT adds `(spawned − completed) × Δt` for the step. Link delay uses the event times `departure − entry` and is clamped at zero. The two clocks agree up to an \(O(\Delta t)\) discrepancy per vehicle, which is visible on sub-step links.
6. **Merge order.** Shared downstream space is offered to approaches in ascending edge id within the step.
7. **Multilane links** are one FIFO reservoir. A turning vehicle at the head holds the through movement.
8. **Yield.** Minor approaches under flashing amber or an unsignalized priority node take a constant \(\tau = 4.5\) s, and they receive no discharge slot in a step where a major vehicle is ready.
9. **No U-turn off a closure.** A vehicle already on a zero-capacity link waits there until capacity returns. Drivers upstream of that link, at a node with another exit, divert even when less than 300 m remain.
10. **Bounded rationality is not Wardrop.** The threshold is the scenario inertia (default 1.4) against the driver’s current free-flow continuation, with a 90 s cooldown between searches. Paths shorter than 300 m are kept under ordinary delay; a severed remaining edge is exempt from that gate.
11. **Demand** in these tests is a boundary rate or an explicit trip list. It is not a calibrated origin–destination matrix, so a GEH statistic against counts is not defined. GEH here is between numerical replicates, where it is zero.

## Remediation

The audit found three defects that could drop a closure, freeze a queue, or propagate a non-finite credit. All three are fixed in `crates/sim-engine/src/engine.rs`. The suite was run again after the fixes: `scientific_audit` 14 tests passed, `physics` 6 tests passed, `sim-engine` unit tests 10 passed.

1. **Departure search ignored closures.** A new trip was routed on free-flow time, so a capacity-0 link still won when it was shorter. The release step now searches free-flow weights with zero-capacity edges removed. If every path is severed, it falls back to the geometric shortest path so the trip queues instead of being discarded.
2. **The 300 m gate hid a severed link.** A driver inside the last 300 m would not look for another exit, so a 120 m closure held the queue even when a parallel link existed. A remaining edge with a non-finite experienced weight now forces a search at any node that has two or more exits.
3. **Route weights went stale after a scenario edit.** Discharge already read the live scale, but the experienced-time vector was refreshed only when a queue changed. `scenario_mut` marks the weights dirty, and the next step rebuilds them before trips are released and before movements are proposed. Reopening the spillback fixture still clears.
4. **Non-finite discharge.** `discharge_slots` treats a non-positive or non-finite capacity, step, or credit as a closed approach and returns a finite remainder. The per-step audit rejects non-finite VHT, VKT, delay, or discharge credit.

A negative capacity scale is already clamped to zero in `scale_factors` and now behaves as a closure, which the severance test checks.

## Reproducing the run

From the repository root, with `data/cache/thessaloniki.simnet` present:

```text
cargo test --test scientific_audit -- --nocapture
```

Each test prints one `AUDIT test=...` line. A failing invariant returns a non-zero exit code. The Thessaloniki cache is gitignored; without it, `thessaloniki_sample_conserves_mass_and_repeats` cannot run. The other thirteen tests build their own networks and do not need the cache.
