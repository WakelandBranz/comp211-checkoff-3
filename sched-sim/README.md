# sched-sim

A tiny, deterministic Rust simulator that runs the same workload on one simulated CPU under four Linux schedulers:

| Scheduler | Core idea modeled |
|---|---|
| Linux 0.01 (1991) | Per-task `counter`; run the biggest; refill with `counter/2 + priority`; no wakeup preemption; HZ=100 |
| O(1) (2.6.0, 2003) | Active/expired priority arrays, 140-bit bitmap, nice-based timeslices, `sleep_avg` interactivity bonus |
| CFS (2.6.23, 2007) | vruntime tree (`BTreeSet`), leftmost wins, weighted dynamic slice, sleeper credit, wakeup granularity |
| EEVDF (6.6, 2023) | Weighted average vruntime, lag/eligibility, fixed slice → virtual deadline, RUN_TO_PARITY |

No dependencies, only std.

## Run

```
cargo run --release
```

## Workload (edit `workload()` in `src/main.rs`)

- **A**: CPU hog, nice 0
- **B**: CPU hog, nice +5
- **C**: interactive task that runs 1 ms, then sleeps 9 ms (like a game loop or an editor)

The simulation runs 10 s. Stats skip the first 1 s of warm-up.

## Sample output

```
FAIRNESS — share of total CPU time
scheduler                   A        B        C   A:B ratio
Linux 0.01 (1991)       59.6%    39.6%     0.8%        1.51
O(1) (2.6.0, 2003)      51.7%    38.3%    10.0%        1.35
CFS (2.6.23, 2007)      67.8%    22.2%    10.0%        3.06
EEVDF (6.6, 2023)       68.7%    22.5%     8.8%        3.06
  Target A:B ratio from nice weights: 1024 / 335 = 3.06

RESPONSIVENESS — time waiting before getting the CPU (ms)
scheduler                C avg   C worst   A worst   B worst    switches
Linux 0.01 (1991)        115.0     140.0     101.0     151.0         144
O(1) (2.6.0, 2003)         0.0       0.0      84.0     112.0        1882
CFS (2.6.23, 2007)         0.0       0.0       2.0       8.0        3797
EEVDF (6.6, 2023)          1.3       3.0       2.0       5.0        5304
```

It also prints a 100 ms timeline (1 character = 1 ms) showing which task had the CPU.

## What each result shows

- **Linux 0.01:** There's no wakeup preemption, and the hogs run for 100–150 ms at a time. C waits more than 100 ms every time it wakes, so it gets 0.8% of the CPU even though it wants 10%. Nice is crude: priority 15 vs 10 gives A:B = 1.5.
- **O(1):** The `sleep_avg` heuristic correctly spots that C is interactive, so C runs instantly. But nice only changes timeslice length (100 ms vs 75 ms), so A:B = 1.35 instead of the intended ~3. The hogs still wait around 100 ms.
- **CFS:** Weighted vruntime hits the target A:B ratio of 3.06 exactly, with no guessing involved. Waits drop to a few ms. In this workload, its sleeper credit makes C instant.
- **EEVDF:** Fairness is the same as CFS, and worst-case waits are a bit tighter for every task. The cost is more context switches and about 1 ms of latency for C, because the running task finishes its 0.75 ms slice first (RUN_TO_PARITY).

Context switches go up each generation. That's the trade: more switching buys shorter waits.

## Simplifications (state these in the talk)

These are models of each algorithm's main idea, not the real kernels.

- One CPU, so there's no load balancing, cgroups, or SMP effects.
- Time advances in 100 µs steps, and preemption checks happen only at timer ticks or wakeups (no HRTICK).
- The kernel version details are the original versions. For example, EEVDF is modeled as it was in 6.6, without later features such as DELAY_DEQUEUE or per-task custom slices.
- Constants are the real 1-CPU defaults: CFS latency 6 ms, minimum granularity 0.75 ms, wakeup granularity 1 ms; EEVDF base slice 0.75 ms; nice-to-weight table from `kernel/sched/core.c`.
