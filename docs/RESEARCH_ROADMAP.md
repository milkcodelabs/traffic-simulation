# Research Roadmap

**Date:** 12 October 2026
**Baseline:** Scientific audit in `docs/SCIENTIFIC_AUDIT.md` (verdict: conditionally valid spatial-queue engine)
**Network used for sizing:** compiled Thessaloniki cache, 16,937 nodes, 38,313 edges, 2,009 signals, 147 routable entry gateways, 144 routable exits

This document is an engineering proposal. It names the modules and the equations a later implementation has to satisfy. It does not claim those features exist.

## Executive synthesis

The platform today is a deterministic mesoscopic assignment model with a desktop viewer. A compiled graph (`sim-core`), a one-second spatial-queue step (`sim-engine`), an OSM compiler (`sim-ingest`), and a binary frame to the Tauri map (`sim-ipc`) already support diurnal boundary demand, bounded-rationality rerouting, fixed and flashing signals, closures, speed scales, and paired Monte Carlo diffs (`RunTotals` of VHT, VKT, and delay). On the audit fixtures, mass residual is zero, identical seeds match to the bit, and a 250% overload clears once demand stops.

That is a strong shell for controlled experiments. It is not yet a workbench a transportation researcher can point at a city and defend. Four gaps dominate, and they are the reason the proposals below are ordered the way they are.

1. **Demand is not an origin–destination matrix.** `DiurnalProfile` is twenty-four hourly fractions. Trips are emitted at gateways by `emit_seeded_gateway_trips`. Zone scales multiply a rate. There is nothing to calibrate against a loop count.
2. **The step is one reservoir per link.** Outflow after free-flow sits at capacity. Inflow in one step can fill residual storage (the audit measured a 40-vehicle burst into a link whose capacity is 0.444 veh/s). Grade, idle time, and link speed exist as observables. A fundamental-diagram speed \(v(k)\) inside the link does not.
3. **Routing cost is time.** `free_flow_weights` and `experienced_weights` are seconds. Tolls, fares, and a value of time have nowhere to sit without contaminating `delay_s`, which is defined as time beyond free-flow.
4. **Every vehicle is the same class.** There is no bus, no EV, and no signal action other than the compiled cycle and the night-flashing window.

### Where this sits against other tools

| Tool | What it already does that we do not | What we can offer without copying it |
|---|---|---|
| **DynaMIT** | Mesoscopic supply plus online OD estimation from sensor counts. This is the closest relative. | A bit-exact supply model. Two SPSA evaluations that share a seed differ only in the demand parameters, which is the property an online estimator wants and a stochastic microsimulator has to average away. |
| **MATSim** | Agent plans, activity scoring, co-evolutionary replanning, and a contrib emissions package over a full day. | A one-second within-day queue with spillback and signals. MATSim's typical time step does not resolve a 4.5 s yield or a 90 s reroute cooldown. We should not grow an activity-based population until the gateway OD is identifiable. |
| **SUMO** | Car-following, TraCI, HBEFA emissions, and `.net.xml` as a lingua franca. | Network interchange and a step socket. Importing SUMO's microsimulation state, or claiming HBEFA traffic situations we cannot observe, would launder a queue model as a car-following model. |
| **A/B Street** | Interactive OSM editing, signal staging, and a planning-scale UI. | We already diff two runs (`sim-engine/src/diff.rs`) and stream bottlenecks. The missing piece is policy instruments inside the cost and the signal, not another map editor. |

Any new physical output has to be a function of quantities the step actually produces: event travel time \(L_e / t^{\text{spent}}\), idle time already stored in `edge_delay_s`, occupancy \(N_e\), storage \(S_{max,e}\), and the compiled grade \(\theta_e\) on `Edge`. Reconstructing a congested fundamental diagram for emissions or safety would contradict the audit.

## Design rules for every proposal

- **Same seed, same bytes.** Calibration and RL training keep the departure seed fixed across a paired evaluation. The audit measured a coefficient of variation near 0.01 from seed noise alone. That noise is acceptable as a reported sensitivity. It is not acceptable as an untracked component of a finite-difference gradient.
- **Delay stays in seconds.** Money, energy, and emissions are extra accumulators beside `delay_s`, `vht_s`, and `vkt_m`. They must not be folded into `habitual_s`.
- **A scenario edit is visible on the next step.** `scenario_mut` already marks route weights dirty. New scales (toll, commanded speed, bus-lane closure) use that path rather than a second cache.
- **Cycle length is conserved when a signal is edited in real time.** The audit's mode boundary moved no vehicles in the boundary step. Transit priority and RL actions add and remove green inside a fixed cycle. They do not insert a lump of discharge credit.
- **Thessaloniki is the sizing case.** 38,313 edges and 2,009 signals. A feature that allocates a per-vehicle object per second, or that runs Dijkstra from every signal every second, does not ship.

---

## A. Environmental, energy, and emissions

### A1. Link-averaged hot exhaust and idle

**Theoretical formulation.** Use an average-speed hot factor of COPERT form, evaluated at the speed the queue model actually drove, plus an idle term at the delay the stepper already records. For pollutant \(p \in \{\mathrm{CO}_2, \mathrm{NO}_x, \mathrm{PM}\}\):

\[
E_{p,e} = Q_e \, L_e \, \mathrm{ef}_p\!\left(v_e, \theta_e\right) + r^{\text{idle}}_p \, t^{\text{idle}}_e
\]

\[
v_e = \frac{L_e}{\bar t_e}, \qquad \bar t_e = \frac{\texttt{edge\_travel\_s}[e]}{n^{\text{dep}}_e}, \qquad t^{\text{idle}}_e = \texttt{edge\_delay\_s}[e]
\]

The hot factor is the COPERT rational function of mean speed (Ntziachristos & Samaras), with a grade correction that is 1 on the flat and linear in slope percent for the heavy share:

\[
\mathrm{ef}_p(v, \theta) = \frac{\alpha_p v^2 + \beta_p v + \gamma_p + \delta_p / v}{\varepsilon_p v^2 + \zeta_p v + \eta_p} \cdot \big(1 + \kappa_p \, s \, \phi_H\big)
\]

where \(s = 100 \tan \theta_e\) is the signed slope in percent, \(\phi_H\) is `DEFAULT_HEAVY_VEHICLE_SHARE` (0.08) unless a fleet file overrides it, and \(v\) is floored at a small positive speed so the \(\delta / v\) term stays finite. \(\mathrm{PM}_{2.5}\) and \(\mathrm{PM}_{10}\) add a non-exhaust term proportional to VKT only, \(e^{\text{ne}}_p L_e Q_e\), because brake and tyre wear in the inventory methods are not an idle process.

Coefficients live in a versioned table (`copert-eu-pc-euro6` or an HBEFA situation mapped onto the same speed bins). The functional form is fixed. The table is data, so a paper can name the inventory version.

Do not feed \(v(k)\) from `equilibrium_flow` into \(\mathrm{ef}\). On the congested branch that curve and the stepper disagree by 0.341 veh/s RMSE in the audit's diagram check. The speed argument is \(L / t^{\text{spent}}\), which is below free-flow exactly when the vehicle queued.

**Architectural integration.**

- Pure functions in `sim-core/src/emissions.rs`: `hot_factor(pollutant, speed_mps, grade_rad, fleet) -> g_per_m` and `idle_rate(...) -> g_per_s`. No graph, no clock. Unit tests check the flat-road limit \(\theta = 0\), positivity, and a finite value as \(v \to 0\).
- `Edge` already carries `grade_rad`, `length_m`, and `uphill_grade_rad`. No schema change for the average-speed model.
- Accumulation sits next to the existing delay update in `Simulation::apply`, where a departure already knows `spent`, `length`, and `edge_delay_s`. One addition per departure: \(L \cdot \mathrm{ef}(L/\text{spent}, \theta)\) plus idle rate times `(spent - free_flow).max(0)`.
- `RunTotals` and the experiment CSV gain `co2_g`, `nox_g`, `pm25_g`, `pm10_g`. `EdgeObservation` gains the same four, so `sim-engine/src/diff.rs` can color \(\Delta \mathrm{CO}_2\) with the existing improved / degraded scale.
- The desktop frame stays a scalar in `FrameHeader` (network grams this step). Per-link emissions are a research export, not a 60 Hz layer. 38,313 extra `f32`s would be cheap; streaming them every frame is not the first use.

**Research utility.** "Does a cordon that cuts delay on the ring increase \(\mathrm{NO}_x\) on the parallel arterials?" is a paired run the batch runner already knows how to difference, once both arms accumulate grams. The hypothesis is about the sign of \(\Delta E_p\) conditional on \(\Delta \text{Delay}\), which the current VHT-only output cannot test. A second hypothesis, specific to this supply model: because idle is `(spent - free_flow)` and a closure freezes a queue without deleting vehicles, a bottleneck scenario must raise idle emissions even when VKT falls. If it does not, the accumulator is wrong.

### A2. Electric-vehicle energy on grade, with regeneration

**Theoretical formulation.** Traction energy for a vehicle of mass \(m\) on link \(e\), positive grade uphill in the direction of travel:

\[
F_e = m g \big(\sin \theta_e + C_{rr} \cos \theta_e\big) + \tfrac{1}{2} \rho C_d A \, v_e^2
\]

\[
W_e =
\begin{cases}
F_e L_e / \eta_{\text{trac}} & F_e > 0 \\
\eta_{\text{regen}} \, F_e L_e & F_e < 0
\end{cases}
\]

Regeneration is capped by the potential that can actually be returned, and by a cutoff speed below which friction brakes are assumed to take the stop: if \(v_e < v_{\text{cut}}\) and the vehicle is in the idle portion of the link, regen on that departure is zero. Fleet energy is the departure sum times an EV share. A single battery state of charge is justified only for a tracked transit vehicle (section C); the car fleet is an integral of \(W_e\), not \(10^5\) SOC integrators.

Ano Poli and the Panorama descent are the local test. Those edges already store `uphill_grade_rad` and signed `grade_rad` from `grades_from_profile`. An eastbound ascent and the opposing descent are two directed edges, so the ascent pays \(mg\sin\theta \, L / \eta\) and the descent returns \(\eta_{\text{regen}} mg\sin|\theta| L\). A round trip does not net to zero. The ratio of returned energy to spent potential is \(\eta_{\text{trac}}\eta_{\text{regen}} < 1\), which is the check the unit test should lock.

**Architectural integration.** Same departure hook as A1, behind a fleet record `{share_ev, mass_kg, c_rr, cd_a, eta_trac, eta_regen, v_cut}`. Default share 0 leaves the accumulator at zero so existing experiments do not change. `RunTotals.ev_kwh` is the export. The map does not need a layer until a researcher asks for kWh per edge, which reuses the diff color ramp.

**Research utility.** Whether an EV fleet share reverses the emissions ranking of a steep bypass against a longer flat arterial. Delay can prefer the short steep road while energy does not. That comparison is impossible while cost and energy are the same number.

---

## B. Counts, OD estimation, and probe trajectories

### B1. Screenline counts and a bilevel OD

**Theoretical formulation.** The lower level is the stepper we have: bounded-rationality paths, not a static user equilibrium. Given a demand parameter \(\theta\), one seeded run produces link departures \(Q_e(\theta)\) per hour from the cumulative outflow the step already increments.

The upper level fits \(\theta\) to counts \(c_{e,h}\). The loss is the GEH form used in assignment validation, summed over counted links and hours:

\[
J(\theta) = \sum_{e \in \mathcal{C}} \sum_{h=0}^{23} \frac{2 \big(Q_{e,h}(\theta) - c_{e,h}\big)^2}{Q_{e,h}(\theta) + c_{e,h}}
\]

with the convention that a term is zero when both the count and the model are zero.

A free \(147 \times 144\) gateway matrix is about \(2.1 \times 10^4\) cells. SPSA on that many cells, each pair of evaluations a city-day, is a cluster job wearing a research label. The first estimator is a doubly constrained entropy (gravity) model with far fewer free numbers:

\[
q_{od} = a_o \, b_d \, T_o \, A_d \, \exp(-\beta \, \tau_{od})
\]

\(\tau_{od}\) is the free-flow time of the current router (`shortest_path` on `free_flow_weights`). Balancing factors \(a_o, b_d\) are solved by iterative proportional fitting so that productions and attractions match. The free parameter that the counts identify is \(\beta\), plus a handful of corridor multipliers already shaped like `ZoneScale`. SPSA, or a one-dimensional line search, is then over \(\beta\) and those multipliers:

\[
\hat g_i = \frac{J(\theta + c \Delta) - J(\theta - c \Delta)}{2 c \Delta_i}, \qquad \Delta_i \in \{-1,+1\}
\]

Both evaluations use the same departure seed. The audit's bit-exact repeat is what makes \(\hat g\) a property of \(\theta\) rather than of the RNG. Step sizes follow the standard SPSA schedules \(a_k = a/(k+A)^\alpha\), \(c_k = c/k^\gamma\), with \(\alpha = 0.602\), \(\gamma = 0.101\).

Counts enter as a CSV: OSM way id, hour, vehicles, optional direction. Join through `Edge.osm_way_ids`. A way that was contracted into several edges splits the count in proportion to length, and the loss is applied to the sum, not to each fragment, so contraction does not invent precision.

**Architectural integration.**

- `sim-core`: `OdProblem { productions, attractions, beta, corridor_scale }` and `gravity_od(...) -> Vec<Trip>` at the gateway resolution. `DiurnalProfile::urban_default` still shapes the clock; it multiplies \(q_{od}\), it does not replace it.
- `sim-engine/src/demand.rs`: stop treating "vehicles per hour at the cordon" as the scientific demand. Keep `emit_seeded_gateway_trips` as the uncalibrated baseline the audit tested. Add `emit_od_trips(matrix, seed, profile, cursor)`.
- `sim-cli`: `calibrate --counts counts.csv --seed 1 --iters 40`. Each iteration is two `advance` calls. Write \(J\), \(\beta\), and GEH percentiles to the experiment directory that `batch.rs` already uses.
- Do not put the optimizer inside `step`. The lower level must stay the deterministic function the audit certified.

**Research utility.** The hypothesis a reviewer will ask for: after calibration, GEH on held-out screenlines is below 5 for the peak hours, and the same \(\beta\) does not fit the night hours (the night flashing regime changes travel times). A negative result is informative. It would mean the gravity structure cannot carry this network's counts and the next estimator needs a richer \(\theta\), which is a P3 cell-level SPSA on a reduced active set, not a reason to start there.

### B2. Floating-car travel times as a check, not a nudge

**Theoretical formulation.** Probe points \((t, \lambda, \varphi)\) are map-matched in the existing UTM frame (`UtmProjection`) to the nearest directed edge whose geometry contains the point within a match radius, subject to continuity along the trajectory. The empirical travel time on edge \(e\) is the difference of matched timestamps on entry and exit. The simulated counterpart is \(\bar t_e\) from A1.

Agreement is RMSE and Theil's \(U\) on the paired means, the same \(U\) the audit reported for route shares (0.061 on the diverge test). GEH is the wrong statistic for times. A Kolmogorov–Smirnov distance on the distribution is reported only when a link has at least 30 probe traversals; below that the test has nothing to say and the report must show the count.

The first pipeline does not write speeds back into `experienced`. Assimilating probe speeds as a capacity scale would make the supply model a slave of the trace and would void the conservation argument, which assumes the discharge law rather than an external speed.

**Architectural integration.** A reader in `sim-ingest` produces `Vec<ProbeTraverse { edge, enter_s, leave_s }>`. A function in `sim-engine` joins those to `observations()`. Output is a CSV next to `summary.csv`. The desktop can list the worst Theil links in the bottleneck table by reusing `top_bottlenecks`; that is a UI column, not a new layer.

**Research utility.** Separate the two failure modes the audit cannot see: a supply model that is dynamically consistent and still has the wrong travel-time distribution because the OD is wrong, versus a supply model whose queue discharge (44 vehicles against a capacity integral of 44.44 in the audit) is fine while a corridor's probes say otherwise. Those two outcomes point at B1 or at the single-cell approximation, respectively.

---

## C. Transit

### C1. A second vehicle class, bus lanes, and dwell as a FIFO hold

**Theoretical formulation.** General traffic and transit do not share a weight vector.

- On a dedicated lane, car weights are \(+\infty\) (the same sentinel a capacity-0 closure already uses) and transit weights are the free-flow time of that lane.
- On a mixed lane, a dwell of \(d\) seconds withholds discharge slots. `discharge_slots` already returns zero slots while an approach is closed and keeps the credit finite. A dwelling bus is that closure, held for \(d\) seconds, then released. Followers on a one-lane reservoir wait. That is the shockwave this supply model can represent. The audit showed a blocked head does not let the queue reorder.
- Effective mixed-lane capacity during a stop with \(n\) remaining general-purpose lanes is the existing `closure_scale(lanes, 1)` for the dwell window only.

The overstatement on multi-lane mixed streets is the one the audit already named: one FIFO reservoir means the bus holds every lane. The implementation must record `lanes` on the edge and apply the one-lane scale when `lanes > 1`, rather than closing the whole link. Dedicated lanes are a separate directed edge, or a `transit_only` bit, so cars never enter them.

**Architectural integration.**

- `Trip` and `Vehicle` gain a `class: VehicleClass` (`General` or `Transit`). Path search takes the weight vector for that class. `experienced_weights` becomes two slices, or one vector with transit edges appended. Prefer two `Vec<f64>` of length \(E\) so Dijkstra stays as it is.
- Stops are a small table `{ node, dwell_s, edge }` compiled in `sim-ingest` from OSM `highway=bus_stop` matched to the nearest through edge. The stepper, on a transit vehicle reaching that node, sets a per-edge dwell deadline. General traffic on a mixed edge sees `approach_open == false` until the deadline, which is the mechanism flashing amber already uses to suppress a minor approach.
- `EdgeFlags` gains `transit_lane: bool`. OSM `busway` / `lanes:psv` set it at compile time.
- UI: a scenario toggle, same path as lane closures in `overlay.rs`. Not a new renderer.

**Research utility.** The policy question on the Thessaloniki arterials is whether a curbside bus lane reduces transit delay by more than it increases general-traffic delay on the same corridor. Both delays are already `edge_delay_s`, split by class once class exists. The hypothesis is a sign test on \(\Delta \text{Delay}_{\text{transit}}\) and \(\Delta \text{Delay}_{\text{general}}\) in a paired batch.

### C2. Transit signal priority without growing the cycle

**Theoretical formulation.** A transit vehicle on an incoming edge of signal \(n\), with remaining free-flow time below a detection horizon \(\tau_{\text{det}}\), requests the phase that already lists that edge.

Let the current phase green be \(g\) and the opposing green \(g'\), with \(g + g' = G\) fixed (yellow and all-red stay put). An extension of \(\Delta\) seconds is feasible when the opposing phase still has the compiled minimum green (`MIN_GREEN_S` is 8 s):

\[
\Delta^\star = \min(\Delta_{\max},\, g' - g_{\min}), \qquad g \leftarrow g + \Delta^\star, \qquad g' \leftarrow g' - \Delta^\star
\]

Early green is the same transfer applied to the following phase when the bus phase is next. The cycle length `cycle_length_s` does not change, so `phase_open_at` stays a continuous function of clock time plus a stored offset. Discharge credit is not reset. That is the condition the 06:00 audit was checking: occupancy does not jump by more than one step of capacity.

**Architectural integration.** A `PriorityOverride { until_s, green_delta }` on the signal, consulted by `phase_open_at` and cleared when `until_s` passes. No change to `plan_signal`. The desktop scenario panel gets a TSP on/off next to flashing amber. Detection uses the transit class from C1, so TSP does not ship before the class exists.

**Research utility.** Whether priority on the east–west approaches of a corridor cuts transit delay without moving the network GEH of general traffic past a pre-registered threshold. The outcome is a number in the existing diff, not a new theory of signals.

---

## D. Pricing, speed control, and safety indicators

### D1. Tolls in the routing cost, not in the delay

**Theoretical formulation.** The user-facing cost, in time units, is

\[
C_e = t_e + \frac{\pi_e}{\lambda}
\]

where \(t_e\) is the free-flow or experienced time already computed, \(\pi_e\) is money, and \(\lambda\) is the value of time in money per second. This is the stated form \(C_e = t_e + \beta \pi_e\) with \(\beta = 1/\lambda\).

Two instruments share the equation.

- **Cordon.** \(\pi_e = \pi_{\text{cross}}\) on a listed set of directed edges and 0 elsewhere. The Thessaloniki ring is already selected by `is_ring_or_motorway` together with `text_is_ring` (the matcher looks for "ring" and "περιφερ"). A cordon is a subset of those edges, not a hardcoded way id.
- **Distance.** \(\pi_e = \rho L_e\).

`should_reroute` compares costs in one unit. It keeps working if both the experienced vector and the baseline vector are \(C\), not a mixture of \(C\) and \(t\). Delay reported to the user remains \(t^{\text{actual}} - t^{\text{free}}\). Toll revenue is \(\sum \pi_e\) over departures, a separate field.

**Architectural integration.** `Scenario` gains `value_of_time` and `edge_toll: Vec<EdgeScale>` (money per traversal, or a per-metre rate flagged on the scenario). `free_flow_weights` and `experienced_weights` add \(\pi_e / \lambda\) after the time term. A zero toll leaves the vectors identical to today, which is the regression the inertia tests must keep passing. `RunTotals.toll_revenue` joins the CSV. The diff view can color \(\Delta C\) only in a mode that is labeled cost, so it is not mistaken for a speed change.

**Research utility.** The elasticity question: at what \(\pi_{\text{cross}}\) does the bounded-rationality split on a ring-versus-arterial diverge move by more than the one flip the untolled audit observed? Because inertia is 1.4, a toll smaller than \(0.4 \, \lambda \, t_e\) must not move a driver who is otherwise at free-flow. That inequality is a unit test before any city run. It is also the policy statement: a cordon below the indifference threshold is revenue without reassignment.

### D2. Variable speed limits on the ring, with hysteresis and a matching capacity

**Theoretical formulation.** Command a speed on ring and motorway edges when density crosses a threshold, and restore it only after density falls below a lower threshold. A single threshold oscillates, which is the failure mode the route-share audit was written to catch.

\[
v^{\text{cmd}}_e(t) =
\begin{cases}
v^{\text{low}} & k_e / k_{\text{jam}} \ge \kappa_{\text{on}} \\
v^{\text{posted}} & k_e / k_{\text{jam}} \le \kappa_{\text{off}} \\
v^{\text{cmd}}_e(t-\Delta t) & \text{otherwise}
\end{cases}
\]

with \(\kappa_{\text{off}} < \kappa_{\text{on}}\). Density is \(N_e / S_{max,e}\), the ratio `write_link_fields` already clamps into \([0,1]\).

The triangular capacity is homogeneous in free-flow speed when the jam density and wave speed are held fixed:

\[
q_c(v) = v \, k_{\text{jam}} \, w / (v + w)
\]

A commanded speed that scales time and leaves `capacity_veh_per_s` alone will lengthen travel time without binding the discharge. The controller therefore writes both a speed scale \(v^{\text{cmd}} / v^{\text{posted}}\) and a capacity scale \(q_c(v^{\text{cmd}}) / q_c(v^{\text{posted}})\), through the existing `EdgeScale` lists, and relies on the dirty-weight rebuild. Edges are those `is_ring_or_motorway` already returns, which is how a lane closure on the Periferiaki is expressed today.

**Architectural integration.** A function `apply_vsl(sim)` at the end of `step`, after `note_saturation`, writing scales only when a threshold is crossed so the dirty flag does not fire every second. `scaled_speed` and `scaled_capacity` then do the rest. The UI control is a threshold pair, not a per-edge editor.

**Research utility.** Whether a drop to a lower commanded speed on the ring postpones the time at which `max_density` hits the storage ceiling, relative to the 0.9937 peak in the 250% audit. The pre-registered outcome is that time-to-storage, plus total delay. If delay rises while time-to-storage does not move, the instrument is not earning its place on this supply model.

### D3. Surrogate safety from the observables we have

**Theoretical formulation.** Time-to-collision between a pair of vehicles requires positions inside the link. This stepper stores a FIFO order and a reservoir count. Pairwise TTC would be invented. The indicators that are functions of the state are:

\[
I^{\text{cv}}_e = \frac{\sigma(v_e)}{\bar v_e}
\]

over a trailing window of link speeds \(v_e(t) = L_e / \max(t^{\text{free}}, N_e / q_c)\), and a breakdown flag

\[
B_e(t) = 1 \quad \text{when} \quad N_e(t) = \lfloor S_{max,e} \rfloor \ \text{and} \ N_e(t-\Delta t) < \lfloor S_{max,e} \rfloor
\]

The audit's spillback case is the calibration of \(B_e\): the feeder reached storage at \(t = 73\) s and did not oscillate. A third series, the one-step speed drop \((v_e(t) - v_e(t-\Delta t))/\Delta t\), marks the onset. Large negative values coincident with \(B_e = 1\) are the mesoscopic shock. They are not a crash probability. A paper that quotes them as SSAM conflicts is misusing the model.

**Architectural integration.** Offline, in `sim-engine`, over the per-edge speed samples the batch fold already keeps (`LinkFold`). No change to `step`. A column on the bottleneck table (`top_bottlenecks`) can sort by \(I^{\text{cv}}\) instead of queue. Memory is one extra running moment per edge if the window is computed online, which is two `f64`s per edge (about 600 KB on Thessaloniki) and can stay off unless the export is requested.

**Research utility.** Whether the links with the highest \(I^{\text{cv}}\) under the 250% pattern are the same links whose delay increased in the flashing-amber diff. If the sets are disjoint, the safety proxy and the delay proxy are different instruments and the UI must not present them as one ranking.

---

## E. Learning interfaces and file formats

### E1. A signal environment for reinforcement learning

**Theoretical formulation.** The environment transition is `Simulation::step(1)`. For a controlled subset \(\mathcal{N}\) of signals (a corridor, not all 2,009):

\[
s_n = \big( \text{phase index},\, \tau_{\text{in phase}} / C_n,\, (N_a / S_{max,a})_{a \in \text{in}(n)},\, \text{spill flag}_a \big)
\]

\[
a_n = \Delta g_n, \qquad |\Delta g_n| \le \Delta_{\max}, \qquad \sum_{\text{phases}} g = G_n
\]

The cycle-sum constraint is the same one as transit priority. The reward for a step is the negative of the delay increment the stepper just added, optionally plus a weight on the sum of approach queues. Because the transition is deterministic given the seed and the action sequence, a published policy can be replayed by feeding the same actions. That is the reproducibility requirement. A stochastic microsimulator cannot offer it without a confidence interval on every episode.

**Architectural integration.** Prefer the socket we already have.

- `sim-ipc`: a second message beside `encode_frame`, fixed-layout, little-endian. Header, then \(|\mathcal{N}|\) observation records, then a reply of \(|\mathcal{N}|\) green deltas. The map frame stays on its own rate. The learning socket steps only when the client writes an action, so a training loop is not tied to 60 Hz.
- Rust side: `Session` in `src-tauri` already holds the `Simulation`. A headless path in `sim-cli` should serve the same message without the webview, because training does not need the map.
- Python: a small `gymnasium` wrapper around that socket. `Env.reset` reloads the snapshot (`Simulation::restore` already exists). `Env.step` writes deltas and reads the next observation plus the delay increment.
- PyO3 is a later in-process binding of the same structs, for callers who will not pay a socket. It is not the first interface. An in-process Python interpreter linked into the Tauri app is a packaging problem with no research payoff until the socket protocol is stable.

Action space for a first paper is one corridor of signals, on the order of ten nodes, not the city. The observation is a few dozen floats. Sending all 38,313 link speeds every RL step repeats the map frame and teaches the agent nothing the corridor queues do not.

**Research utility.** Whether a policy constrained to fixed cycle length beats the compiled fixed cycle on corridor delay without increasing the audit's spillback peak. The baseline is `SignalSchedule::ForceFixed` on the same seed. If the learned policy wins by changing the cycle length, the constraint was not enforced and the result is void.

### E2. GeoTIFF elevation

**Theoretical formulation.** No new physics. `grades_from_profile` already turns a sequence of elevations into `grade_rad` and `uphill_grade_rad`. `GridElevation` already bilinear-samples a row-major raster and returns `None` on NaN. Bridges and tunnels already pass `ignore_grade` because a ground raster is not the deck.

**Architectural integration.** A decoder in `sim-ingest` that reads a GeoTIFF (the `tiff` crate), builds a `GridElevation` in geographic degrees, and passes it as the `ElevationSampler` the compiler already calls. Missing tiles stay `None` and become flat segments, which is the current contract. This is what makes A2's Ano Poli grades empirical rather than whatever OSM elevation tag happened to be present.

**Research utility.** A sensitivity: emissions and EV energy on the compiled grades versus the same graph with `FlatElevation`. If the city-level \(\mathrm{CO}_2\) difference is below the seed CV of about one percent, grade is not identified at the network total and only corridor-level results should be published.

### E3. SUMO `.net.xml`

**Theoretical formulation.** A network homomorphism, not a dynamics import. SUMO lanes become `lanes` and a speed. SUMO connections become `Turn` with `allowed` taken from the connection permission. A junction of type `traffic_light` becomes an input to `plan_signal` only when the SUMO program is not imported; importing an arbitrary SUMO program is a second step that fills `Phase.incoming`, `green_s`, `yellow_s`, and `all_red_s` directly. Car-following parameters, vehicle types, and detectors are ignored. They have no field on `Edge`.

**Architectural integration.** `sim-ingest` reader next to the OSM XML parser, emitting the same `AssembleInput` the OSM path emits, then the same `realize_edge` and cache writer. The CLI flag mirrors `--osm`. Roundtrip test: a small net.xml builds a graph on which the audit's mass fixture can be replayed (two gateways, one link, vehicles conserved).

**Research utility.** Run the same OD and the same counts through this stepper and through SUMO, and publish the travel-time RMSE. That is the external check B2 cannot provide when the only probe source is the same city the model was fit to.

### E4. OpenDRIVE

**Theoretical formulation.** Sample each OpenDRIVE `planView` geometry (line, arc, spiral) into a polyline at a fixed spacing, then call `realize_edge`. OpenDRIVE elevation profiles fill `elevations_m`, which `grades_from_profile` already consumes. Lane sections become `lanes`. Junctions become nodes and turns.

**Architectural integration.** A separate reader in `sim-ingest`. It should not share code with the SUMO reader beyond `AssembleInput`. OpenDRIVE is a corridor and test-track format. It is not how Thessaloniki enters the model. Priority sits below `.net.xml` because the OSM compiler is the city path and SUMO is the tool people will compare against.

**Research utility.** A closed test track with a known grade profile for A2, where OSM tags are a poor elevation source. Useful once GeoTIFF and the EV accumulator exist. Not useful as a city importer.

---

## Prioritization matrix

Complexity is calendar time for one engineer who already knows this repository. "Engine" means `sim-core` / `sim-engine` / `sim-ingest` / `sim-cli`. "UI" means the Tauri frame and the React panels. Performance is relative to a Thessaloniki day with the current stepper.

| Feature | Academic value | Complexity | Performance impact | Priority |
|---|---|---|---|---|
| A1 Link emissions and idle | High | 1 week, engine. A CSV column is enough; a map layer is optional UI. | Modest. One rational-function evaluation per link departure. Four extra `f64` accumulators per edge. | P1 |
| B1 Entropy OD and count loss | High | 3–4 weeks, engine and CLI. No UI required to publish a GEH table. | CPU-heavy. Each SPSA iteration is two seeded city runs. A gravity \(\beta\) is dozens of runs, not thousands. | P1 |
| D1 Tolls in \(C_e = t_e + \pi_e/\lambda\) | High | 4–6 days, engine. One scenario field and a diff mode in the UI. | Modest. An add into the weight vector that is already rebuilt on a dirty scenario. | P1 |
| B2 Probe travel-time join | High | 2 weeks, ingest plus a report. Map-matching is the risk, not the stepper. | None on the step. Offline join. | P1 |
| D2 Ring VSL with hysteresis | High | 1 week, engine. Threshold control in the existing scenario panel. | Modest, and only if scales are rewritten when a threshold flips. Rebuilding 38k weights every second is unnecessary and should be avoided. | P2 |
| C1 Transit class, lanes, dwell | High | 3 weeks, engine. A scenario toggle in the UI. | Modest if transit vehicles are a small fleet. A second Dijkstra weight vector doubles route memory, not route searches, until a transit trip is released. | P2 |
| E1 RL socket and gymnasium wrapper | High | 2–3 weeks, `sim-ipc` and `sim-cli`, plus a thin Python package. | The simulation itself. The socket should carry corridor observations, not the full edge state, or it will dominate. | P2 |
| C2 Transit signal priority | Medium | 1–2 weeks after C1, engine. | Modest. A handful of phase-green edits per step on the signals that see a bus. | P2 |
| A2 EV energy and regeneration | Medium | 3–4 days after A1, engine. | Same departure hook as emissions. | P2 |
| E2 GeoTIFF into `GridElevation` | Medium | 3–4 days, ingest. Compile time only. | None at step time. A one-time raster sample during ingest. | P2 |
| D3 Speed-variation and breakdown flags | Medium | 3–4 days, engine report. Optional bottleneck-table column. | None if computed from the batch fold. Two `f64`s per edge if computed online. | P2 |
| E3 SUMO `.net.xml` import | Medium | 2–3 weeks, ingest, plus a conservation replay test. | None at step time. | P3 |
| B1b Cell-level SPSA on a reduced OD | High | After B1 has shown the gravity model is not enough. Weeks of runs. | CPU-heavy. Only an active set of cells, never the dense \(147 \times 144\) matrix as the default. | P3 |
| E1b In-process PyO3 | Medium | 1–2 weeks after the socket protocol has stopped changing. | Avoids a copy. Not worth it while the protocol is still moving. | P3 |
| E4 OpenDRIVE import | Low | 2–3 weeks, ingest. | None at step time. | P3 |

### Suggested sequence

**P1** makes the model answer a question a reviewer can check: counts in, GEH out, emissions and tolls as extra columns on a diff the batch runner already writes. Tolls and emissions do not depend on each other. OD estimation should start as soon as a count file exists, because every later policy run is otherwise an uncalibrated boundary rate.

**P2** adds the instruments that need a second vehicle class or a live controller: buses, priority, ring VSL, the RL socket, probe reports, and a real elevation raster. VSL and TSP both edit the step, so they land after the P1 columns, not before the conservation tests are extended to cover them.

**P3** is interchange and the estimators we should not build until P1 has failed for a stated reason. OpenDRIVE and PyO3 are in that bin on purpose.

### What this roadmap refuses

- A per-vehicle car-following layer "so that emissions look like MOVES operating modes." The audit's supply model would no longer be the one that was tested.
- Online speed nudging from probes inside `step`.
- An unconstrained RL action that changes cycle length.
- Treating \(I^{\text{cv}}\) as a crash count.
- A full-city OD matrix updated by SPSA as the first calibration.

Those shortcuts would each produce a number. None of them would be a number this codebase can currently defend.
