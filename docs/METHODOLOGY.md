# Methodology

**Traffic Simulation 1.0.0**
**Date:** 12 October 2026
**Network used for city figures:** compiled Thessaloniki graph `data/cache/thessaloniki.simnet` (16,937 nodes, 38,313 directed links, 2,009 signals)

This note states the supply model that the code integrates, the intersection and assignment rules around it, and the discretization facts measured in [SCIENTIFIC_AUDIT.md](SCIENTIFIC_AUDIT.md). Equations below are the ones in `sim-core` and `sim-engine`. Where a textbook object is evaluated only at compile time, the text says so.

## 1. Model class

The stepper is a **single-reservoir mesoscopic spatial queue**.

A vehicle is an individual trip with an origin, a destination, a departure time, and a path. On a link it occupies one place in a first-in-first-out reservoir. The link has a free-flow traversal time and a downstream server. It does not have cells, and it does not move a shock at a finite backward wave speed inside the link.

Two clocks act on a vehicle that has entered link \(e\):

1. **Travel-time delay.** The vehicle cannot reach the downstream node before the free-flow time \(L_e / v_e\).
2. **Queue server.** Once that time has elapsed, departure consumes discharge credit accumulated at the link capacity \(q_e\) (vehicles per second), subject to the downstream node’s signal or yield rule and to residual storage on the next link.

Storage is a hard ceiling

\[
S_{\max,e} = n_e \, K_{\mathrm{jam},e}^{\mathrm{lane}} \, L_e,
\]

with \(n_e\) the lane count in the direction of travel. Inflow during a step is at most the residual space:

\[
\text{Inflow}_e(t) \le \max\bigl(0,\; S_{\max,e} - N_e(t)\bigr).
\]

\(N_e\) is an integer vehicle count. When \(N_e = \lfloor S_{\max,e}\rfloor\), further inflow is refused. Vehicles remain in the upstream queue. Nothing is deleted to clear a jam. Completion occurs only when the occupied link’s head node is the trip destination.

Jam spacing is 8.0 m on motorway and trunk classes and 7.5 m otherwise, so the per-lane jam density is the reciprocal, \(1/8.0\) or \(1/7.5\) vehicles per metre. The audit’s residential fixtures use 7.5 m.

The step is \(\Delta t = 1.0\,\mathrm{s}\). That value is the desktop and batch step. It is not adapted to link length.

## 2. Fundamental diagrams

Flow and density are related by \(q = k \, v\) inside each diagram. The diagram that is stored on the network is selected at compile time (`DiagramFamily`). Thessaloniki is compiled with the triangular diagram. Greenshields and Smulders are implemented and tested; they are not a second runtime discharge law.

### 2.1 Triangular (Newell / Daganzo)

For free-flow speed \(v_f\), backward wave speed \(w\), and jam density \(K_{\mathrm{jam}}\) (vehicles per metre of link, lanes included when the diagram is evaluated per link):

\[
k_c = K_{\mathrm{jam}} \frac{w}{v_f + w}, \qquad q_c = v_f \, k_c = v_f \, K_{\mathrm{jam}} \frac{w}{v_f + w}.
\]

\[
q(k) =
\begin{cases}
v_f \, k & 0 \le k \le k_c, \\
w \, (K_{\mathrm{jam}} - k) & k_c < k < K_{\mathrm{jam}}, \\
0 & k \le 0 \text{ or } k \ge K_{\mathrm{jam}}.
\end{cases}
\]

Wave speed in the compiler is 5.56 m/s on motorway and trunk classes and 5.0 m/s otherwise.

On a compiled edge the triangular branch is rebuilt from the **grade-adjusted** free-flow speed and the **penalized** capacity, so the critical density used at runtime is \(k_c = q_e / v_e\) rather than the unadjusted textbook value. The congested limb is then

\[
q(k) = q_e \frac{K_{\mathrm{jam}} - k}{K_{\mathrm{jam}} - k_c}.
\]

### 2.2 Greenshields

Greenshields (1935) is the parabola

\[
q(k) = v_f \, k \left(1 - \frac{k}{K_{\mathrm{jam}}}\right),
\]

zero at \(k \le 0\) and at \(k \ge K_{\mathrm{jam}}\), with a peak \(v_f K_{\mathrm{jam}} / 4\) at half jam. The compiled evaluator multiplies that parabola by \(q_e / (v_f K_{\mathrm{jam}} / 4)\) so the peak matches the grade-adjusted capacity of the edge.

### 2.3 Smulders

Smulders (1990), in the form coded here, is linear in speed on the free-flow side and linear in flow on the congested side. With critical speed \(u_c\) and capacity \(q_c\),

\[
k_c = q_c / u_c,
\]

\[
q(k) =
\begin{cases}
k \left(v_f - (v_f - u_c) \dfrac{k}{k_c}\right) & k \le k_c, \\[1em]
q_c \dfrac{K_{\mathrm{jam}} - k}{K_{\mathrm{jam}} - k_c} & k > k_c.
\end{cases}
\]

The compiled Smulders branch uses the penalized capacity as \(q_c\) and sets \(u_c = v_f \times \texttt{critical\_speed\_ratio}\). The ratio 1 recovers a triangular free-flow limb.

### 2.4 What the step actually sends

The diagram sets \(q_e\) when the edge is realized. Inside `Simulation::step`, a vehicle that has finished its free-flow time is served at \(\min(q_e, \text{available credit})\). On the congested branch the triangular diagram would send \(w(K_{\mathrm{jam}} - k)\), which falls to zero at jam. The queue server does not. It keeps sending \(q_e\) while the downstream link has room.

The audit compared that sending flow with the triangle on 15 congested samples (capacity 0.563 veh/s in that fixture) and recorded an RMSE of **0.341 veh/s**. At jam density the diagram sends nothing; the reservoir still discharges at \(q_e\) if the next link has space. Jam density stops inflow. It does not stop outflow.

Receiving flow is the residual storage, not Newell’s \(\min(q_c, w(K_{\mathrm{jam}} - k))\). The audit put 40 vehicles into an empty link in one second whose capacity is 0.444 veh/s. That burst is 90 times the one-second capacity and exactly fills the storage of 40.

These two facts are why the audit’s verdict is **conditionally valid** as a spatial queue and not a kinematic-wave certification. Mass is conserved. The congested fundamental diagram is not the discharge law.

## 3. Grade

Longitudinal grade is the length-weighted mean of segment slopes. A missing elevation sample contributes a flat segment, so unknown height does not invent a slope. Bridges and tunnels set `ignore_grade` because a ground raster is not the deck. The bundled Thessaloniki compile uses a flat elevation sampler, so the shipped graph is numerically flat; the penalty is still part of edge realization and is covered by unit tests.

Let \(\theta^+ = \max(0, \theta)\) be the uphill portion in radians, and let \(i = \tan \theta^+\) be the rise over run. Downhill grades do not increase speed or capacity.

Free-flow speed is multiplied by

\[
f_v = \frac{1}{1 + 0.5 \, i}.
\]

Capacity is multiplied by

\[
f_q = \frac{1}{1 + \alpha \, i}, \qquad \alpha = 1.5\,(1 - h) + 6\, h,
\]

with heavy-vehicle share \(h = 0.08\) (`DEFAULT_HEAVY_VEHICLE_SHARE`). At a 5% grade the tests lock \(f_v = 1/1.025\) and \(\alpha = 1.86\). Jam density is not reduced by grade. The Highway Capacity Manual is the operational reference for treating grade and heavy vehicles as a saturation-flow penalty; the coefficients above are the ones this compiler applies, and a reader should not treat them as a numbered HCM equation.

## 4. Intersection dynamics

### 4.1 Fixed cycle

A signalized node receives a cycle built from its incoming approaches. Opposing quadrants share a phase when one axis strictly outranks the other. Equal-rank axes are split so conflicting movements are not given green together. Green is split by lane count inside a target cycle of 90 s on a major approach and 60 s otherwise, with a minimum green of 8 s.

Each phase then appends

\[
t_{\mathrm{yellow}} = 3.0\,\mathrm{s}, \qquad t_{\mathrm{all\text{-}red}} = 1.5\,\mathrm{s}.
\]

The local clock is \((t + t_{\mathrm{offset}}) \bmod C\). An approach is open during green and yellow of a phase that lists it, and closed during all-red and during other phases. Discharge credit accumulates only while the approach is open. A closed approach does not mint a negative credit and does not jump the queue forward when the phase returns.

### 4.2 Night flashing amber

Every compiled controller carries the window

\[
[00{:}00,\, 06{:}00) = [0,\, 21600)\,\mathrm{s}
\]

from midnight, and a night mode of flashing amber. Outside that window the mode is the fixed cycle. A scenario may override this:

| `signalSchedule` | Effect |
| --- | --- |
| `unchanged` | Use the controller clock, including the night window |
| `forceFixed` | Fixed cycle for the whole horizon. This is `data/presets/thessaloniki_baseline.json` |
| `forceFlashingAmber` | Priority rules for the whole horizon |

`data/presets/thessaloniki_night_amber.json` leaves the schedule unchanged and sets `nightFlashing` to the same six-hour window, which is the paired intervention in `sim-cli experiment`.

Flashing amber and an unsignalized node both use priority control. A minor approach pays a constant gap-acceptance delay

\[
\tau_{\mathrm{yield}} = 4.5\,\mathrm{s}
\]

and receives no discharge slot in a step where a major vehicle is ready at the same node. The delay is that constant, not a draw from a headway distribution. Major approaches are not charged the yield delay.

The audit crossed the 06:00 boundary with vehicles already queued on both the major and the minor approach. The largest occupancy change on any approach in the boundary step was **0**. Trip delay stayed non-negative. The transition does not add or remove a vehicle in order to smooth the mode change.

## 5. Dynamic assignment

### 5.1 Weights

The search is a turn-aware Dijkstra on the successor graph. A link that cannot hold one vehicle (`storage_max_veh < 1`) or whose capacity or speed is zero has infinite weight and is not a route.

Free-flow weight:

\[
t_e^{\mathrm{free}} = L_e / v_e.
\]

Experienced weight at time \(t\), with \(N_e(t)\) vehicles already in the reservoir and \(q_e\) the scaled capacity:

\[
t_e(t) = \max\left(\frac{L_e}{v_e},\; \frac{N_e(t)}{q_e}\right).
\]

Define the queue delay as the non-negative excess of discharge time over free-flow time,

\[
d_e^{\mathrm{queue}}(t) = \max\left(0,\; \frac{N_e(t)}{q_e} - \frac{L_e}{v_e}\right).
\]

Then

\[
t_e(t) = \frac{L_e}{v_e} + d_e^{\mathrm{queue}}(t).
\]

The implementation stores the \(\max\) form (`experienced_weights`). The two expressions are the same quantity. A path cost is the sum of its link weights. Weights are rebuilt when a queue changes and when a scenario edit marks them dirty, so a closure is visible to the next departure search.

### 5.2 Inertia

A driver reroutes only when the experienced cost of the current continuation exceeds the inertia threshold times its free-flow cost:

\[
t^{\mathrm{experienced}} > \iota \, t^{\mathrm{free}}, \qquad \iota = 1.4
\]

(`DEFAULT_ROUTE_INERTIA`). The scenario field `routeInertia` overrides the constant. A value outside the usual 1.3–1.5 band is accepted; the audit pinned one driver at 1000 and that driver stayed on the habitual path.

The search is not repeated for a smaller gap. There is a 90 s cooldown between searches. Paths with less than 300 m remaining are kept under ordinary delay. A remaining edge whose experienced weight is non-finite (a closure) forces a search at a node with two or more exits, including inside that 300 m gate. If every usable path is severed, departure falls back to the geometric shortest path so the trip queues instead of being discarded.

This is a bounded-rationality rule in the sense of an indifference band (Mahmassani and Jayakrishnan, 1991). It is not a Wardrop equilibrium, and it is not the method of successive averages. On the audit diverge, with inertia 1.4, one majority-path flip occurred and Theil’s \(U\) against a lag-1 forecast of the share was 0.061.

Demand for city runs is a diurnal boundary rate at gateway nodes in the giant component, not a calibrated origin–destination matrix. Hourly fractions are a normalized mixture of a night floor and three Gaussians (morning near 08:00, midday near 13:00, evening near 17:30) and sum to 1. Unroutable pairs are refused before spawn and are not part of the conservation census.

## 6. Scenarios and comparison

A scenario does not copy the graph. It scales capacity and speed per edge, scales boundary demand, and selects the signal schedule. Global speed and demand multipliers apply after the per-edge scales. A scale of zero closes the link. A negative scale is clamped to zero.

The desktop and the headless experiment compare a baseline with an intervention. For each link the score is

\[
\frac{\Delta v_e}{v_{\mathrm{free},e}} - \frac{\Delta \mathrm{Delay}_e}{30},
\]

with \(\Delta v_e = v_{e,\mathrm{test}} - v_{e,\mathrm{base}}\) and \(\Delta \mathrm{Delay}_e\) in seconds. Positive is improved (class 1, blue). Negative is degraded (class 2, red). A link is neutral (class 0) when both runs are unused and delay is about zero, or when the speed change stays inside 5% of free-flow (free-flow at least 1 m/s) and the delay change stays inside 1 s.

Headless replicates use seeds `config.seed + (arm << 32) + run`. Summary intervals are Student-t at 95%, with the two-sided critical value tabulated for degrees of freedom up to 30 and 1.96 above that. Ten runs use 2.262.

## 7. Discretization and other limits

The following are properties of this integrator. They were measured, or they are what the code does. Changing them would be a different model. Numbers are from the 12 October 2026 audit unless noted.

1. **Congested sending flow.** Outflow after free-flow sits at capacity. RMSE against the triangle on the audit’s congested samples is 0.341 veh/s. See §2.4.
2. **One cell per link.** Spillback has no internal travel time. The upstream end is blocked in the same step the reservoir fills. The audit’s 150 m feeder has kinematic-wave time \(L / w = 30\,\mathrm{s}\). The stepper does not move a shock along that feeder. It stops inflow when \(N = S_{\max}\) and continues outflow at capacity while the exit is open.
3. **Integer step and short links.** An 8.00 m motorway link at 30 m/s has Courant number \(v_f \Delta t / L = 3.75\). Free-flow time is 0.267 s. The vehicle cannot leave in the step it enters, and the departure stamp is clamped to the next step boundary, so the recorded travel time is 1.000 s and the implied speed is 8.00 m/s. The error is a delay of up to \(\Delta t\), not an overshoot of density or speed. On Thessaloniki the minimum link length is 1.06 m, the maximum Courant number is 11.330, and 3,278 of 38,313 links (8.6%) have \(v_f \Delta t > L\). `sim-cli audit` reprints the length and Courant summary from whatever cache it is given.
4. **Vehicle-hours versus event delay.** VHT adds \((\mathrm{spawned} - \mathrm{completed})\,\Delta t\) for the step. Link delay uses event times and is clamped at zero. The two clocks differ by \(O(\Delta t)\) per vehicle, which is visible on sub-step links.
5. **Merge order.** Shared downstream space is offered to approaches in ascending edge id within the step. Intra-link FIFO held in the audit. Arrival-time priority at an unsignalized merge is not what the node implements.
6. **Multilane links** are one FIFO reservoir. A turning vehicle at the head holds the through movement.
7. **No U-turn off a closure.** A vehicle already on a zero-capacity link waits there until capacity returns.
8. **Demand** in the audited runs is a boundary rate or an explicit trip list. A GEH statistic against counts is not defined. GEH between numerical replicates of the same seed is zero, including a 48-trip Thessaloniki sample and a 70-link parallel case.

Conservation of the census `spawned = active + completed` held on every checked step, including the Thessaloniki sample. Identical seeds match to the bit. A 250% overload of a corridor cleared 1,005 s after demand stopped, with mass residual 0.

## 8. Reproducibility

From the repository root, with the tracked cache present:

```text
cargo test -p sim-engine --test scientific_audit -- --nocapture
sim-cli audit --cache data/cache/thessaloniki.simnet
```

The performance protocol (loads, warmup, 90 timed steps, and the 24-hour run) is [PERFORMANCE_REPORT.md](PERFORMANCE_REPORT.md).

## References

Daganzo, C. F. (1994). The cell transmission model: A dynamic representation of highway traffic consistent with the hydrodynamic theory. *Transportation Research Part B*, 28(4), 269–287.

Daganzo, C. F. (1995). The cell transmission model, part II: Network traffic. *Transportation Research Part B*, 29(2), 79–93.

Greenshields, B. D. (1935). A study of traffic capacity. *Highway Research Board Proceedings*, 14, 448–477.

Lighthill, M. J., & Whitham, G. B. (1955). On kinematic waves II. A theory of traffic flow on long crowded roads. *Proceedings of the Royal Society A*, 229(1178), 317–345.

Mahmassani, H. S., & Jayakrishnan, R. (1991). System performance and user response under real-time information in a congested traffic corridor. *Transportation Research Part A*, 25(5), 293–307.

Newell, G. F. (1993). A simplified theory of kinematic waves in highway traffic, part I: General theory. *Transportation Research Part B*, 27(4), 281–287.

Richards, P. I. (1956). Shock waves on the highway. *Operations Research*, 4(1), 42–51.

Smulders, S. (1990). Control of freeway traffic flow by variable speed signs. *Transportation Research Part B*, 24(2), 111–132.

Transportation Research Board. *Highway Capacity Manual*. National Academies, Washington, D.C. The grade and heavy-vehicle penalty in §3 is this compiler’s formula; the manual is the reference for treating those effects as a saturation-flow adjustment.
