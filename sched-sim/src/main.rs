//! sched-sim — a tiny, deterministic simulator of four Linux CPU schedulers.
//!
//! Runs the SAME workload on one simulated CPU under:
//!   1. Linux 0.01 (1991)  — counter-based round robin
//!   2. O(1)  (2.6.0, 2003) — priority arrays + bitmap + sleep_avg heuristic
//!   3. CFS   (2.6.23, 2007) — lowest vruntime first, red-black tree
//!   4. EEVDF (6.6, 2023)   — eligible (lag >= 0) + earliest virtual deadline
//!
//! These are SIMPLIFIED MODELS of each algorithm's core idea, not the real
//! kernels: one CPU, no cgroups, no SMP load balancing, no I/O devices.
//! Constants follow the real kernels where practical (noted inline).
//!
//! Run with: cargo run --release

use std::collections::{BTreeSet, VecDeque};

// ───────────────────────── simulation parameters ─────────────────────────

/// Simulation granularity: every step is 100 µs of CPU time.
const STEP_US: u64 = 100;
/// Total simulated time: 10 seconds.
const SIM_US: u64 = 10_000_000;
/// Stats ignore the first second so every scheduler is in steady state
/// (e.g. O(1)'s sleep_avg needs time to "learn" which task is interactive).
const WARMUP_US: u64 = 1_000_000;
/// Timeline window printed at the end (starts after warm-up).
const TIMELINE_START_US: u64 = 2_000_000;
const TIMELINE_MS: usize = 100;

// ───────────────────────────── the workload ──────────────────────────────

#[derive(Clone, Copy)]
enum Kind {
    /// Always wants the CPU (e.g. a compile job).
    Hog,
    /// Runs briefly, then sleeps (e.g. a game loop or text editor).
    Interactive { run_us: u64, sleep_us: u64 },
}

#[derive(Clone, Copy)]
struct Spec {
    letter: char,
    desc: &'static str,
    nice: i32,
    kind: Kind,
}

/// Edit this to try other workloads.
fn workload() -> Vec<Spec> {
    vec![
        Spec { letter: 'A', desc: "CPU hog, nice 0", nice: 0, kind: Kind::Hog },
        Spec { letter: 'B', desc: "CPU hog, nice +5 (lower priority)", nice: 5, kind: Kind::Hog },
        Spec {
            letter: 'C',
            desc: "interactive: runs 1 ms, sleeps 9 ms, nice 0",
            nice: 0,
            kind: Kind::Interactive { run_us: 1_000, sleep_us: 9_000 },
        },
    ]
}

// ─────────────────────────── scheduler interface ─────────────────────────

trait Scheduler {
    fn name(&self) -> &'static str;
    /// Timer interrupt period (1 / HZ).
    fn tick_us(&self) -> u64 {
        1_000 // HZ = 1000
    }
    /// Task becomes runnable. `slept_us` is 0 for the initial start.
    fn enqueue(&mut self, id: usize, now: u64, slept_us: u64);
    /// The running task goes to sleep.
    fn block(&mut self, id: usize, now: u64);
    /// The running task was preempted; put it back in the run queue.
    fn put_prev(&mut self, id: usize, now: u64);
    /// Choose (and remove from the queue) the next task to run.
    fn pick_next(&mut self, now: u64) -> Option<usize>;
    /// Charge `ran_us` of CPU time to the running task.
    fn account(&mut self, id: usize, ran_us: u64);
    /// Timer interrupt while `id` is running. Return true to reschedule.
    fn tick(&mut self, id: usize, now: u64) -> bool;
    /// `woken` just woke up while `curr` is running. Preempt `curr`?
    fn wakeup_preempt(&mut self, curr: usize, woken: usize) -> bool;
}

// ═════════════════════════ 1. Linux 0.01 (1991) ══════════════════════════
//
// kernel/sched.c, ~60 lines. Each task has a `counter` (ticks left).
// schedule(): run the runnable task with the biggest counter. If every
// runnable task has counter 0, refill ALL tasks: counter = counter/2 + priority
// (sleepers keep half their unused counter, so they come back "richer").
// No preemption on wakeup: a waking task waits until the current one's
// counter hits 0. HZ = 100 (10 ms ticks), default priority 15 ticks.

struct Linux001 {
    priority: Vec<i64>,
    counter: Vec<i64>,
    runnable: Vec<bool>,
}

impl Linux001 {
    fn new(nices: &[i32]) -> Self {
        // sys_nice() in 0.01 simply subtracted from priority.
        let priority: Vec<i64> = nices.iter().map(|&n| (15 - n as i64).max(1)).collect();
        Self { counter: priority.clone(), priority, runnable: vec![false; nices.len()] }
    }
}

impl Scheduler for Linux001 {
    fn name(&self) -> &'static str {
        "Linux 0.01 (1991)"
    }
    fn tick_us(&self) -> u64 {
        10_000 // HZ = 100
    }
    fn enqueue(&mut self, id: usize, _now: u64, _slept: u64) {
        self.runnable[id] = true;
    }
    fn block(&mut self, id: usize, _now: u64) {
        self.runnable[id] = false;
    }
    fn put_prev(&mut self, _id: usize, _now: u64) {} // stays TASK_RUNNING
    fn pick_next(&mut self, _now: u64) -> Option<usize> {
        loop {
            // Scan from the end of the task array, like the original.
            let (mut best, mut next) = (-1i64, None);
            for i in (0..self.counter.len()).rev() {
                if self.runnable[i] && self.counter[i] > best {
                    best = self.counter[i];
                    next = Some(i);
                }
            }
            if best != 0 {
                return next; // None if nothing is runnable (idle)
            }
            // Everyone runnable is out of time: recalculate ALL tasks.
            for i in 0..self.counter.len() {
                self.counter[i] = self.counter[i] / 2 + self.priority[i];
            }
        }
    }
    fn account(&mut self, _id: usize, _ran: u64) {}
    fn tick(&mut self, id: usize, _now: u64) -> bool {
        self.counter[id] -= 1;
        if self.counter[id] > 0 {
            return false;
        }
        self.counter[id] = 0;
        true
    }
    fn wakeup_preempt(&mut self, _curr: usize, _woken: usize) -> bool {
        false
    }
}

// ═════════════════════════ 2. O(1) (2.6.0, 2003) ═════════════════════════
//
// Two priority arrays (active / expired), each with 140 FIFO lists and a
// 140-bit bitmap. pick_next = find first set bit → constant time.
// Timeslice from nice (100 ms at nice 0). When a slice runs out the task
// moves to `expired`; when `active` empties, the arrays are swapped.
// Heuristic: sleep_avg rises while sleeping, falls while running, and gives
// a ±5 priority bonus. "Interactive" tasks are put back into `active`.

const MAX_SLEEP_AVG_US: u64 = 1_000_000;
const MAX_BONUS: i32 = 10;

struct PrioArray {
    bitmap: [u64; 3], // 192 bits, 140 used
    queues: Vec<VecDeque<usize>>,
}

impl PrioArray {
    fn new() -> Self {
        Self { bitmap: [0; 3], queues: (0..140).map(|_| VecDeque::new()).collect() }
    }
    fn push(&mut self, prio: usize, id: usize, front: bool) {
        if front {
            self.queues[prio].push_front(id);
        } else {
            self.queues[prio].push_back(id);
        }
        self.bitmap[prio / 64] |= 1 << (prio % 64);
    }
    /// The O(1) part: find the first set bit, pop the head of that list.
    fn pop_first(&mut self) -> Option<usize> {
        let word = self.bitmap.iter().position(|&w| w != 0)?;
        let prio = word * 64 + self.bitmap[word].trailing_zeros() as usize;
        let id = self.queues[prio].pop_front();
        if self.queues[prio].is_empty() {
            self.bitmap[word] &= !(1 << (prio % 64));
        }
        id
    }
    fn is_empty(&self) -> bool {
        self.bitmap.iter().all(|&w| w == 0)
    }
}

struct O1 {
    static_prio: Vec<i32>,
    prio: Vec<i32>,
    slice_left: Vec<i64>,
    sleep_avg: Vec<u64>,
    slice_done: Vec<bool>,
    active: PrioArray,
    expired: PrioArray,
    expired_since: Option<u64>,
    nr_running: u64,
}

impl O1 {
    fn new(nices: &[i32]) -> Self {
        let static_prio: Vec<i32> = nices.iter().map(|&n| 120 + n).collect();
        let n = nices.len();
        Self {
            slice_left: static_prio.iter().map(|&sp| Self::timeslice(sp)).collect(),
            prio: static_prio.clone(),
            static_prio,
            sleep_avg: vec![0; n],
            slice_done: vec![false; n],
            active: PrioArray::new(),
            expired: PrioArray::new(),
            expired_since: None,
            nr_running: 0,
        }
    }
    /// 800 ms at nice -20, 100 ms at nice 0, 5 ms at nice +19.
    fn timeslice(static_prio: i32) -> i64 {
        let scale = if static_prio < 120 { 20_000 } else { 5_000 };
        (140 - static_prio) as i64 * scale
    }
    /// static priority adjusted by a -5..+5 bonus from sleep_avg.
    fn effective_prio(&self, id: usize) -> i32 {
        let bonus = (self.sleep_avg[id] * MAX_BONUS as u64 / MAX_SLEEP_AVG_US) as i32 - MAX_BONUS / 2;
        (self.static_prio[id] - bonus).clamp(100, 139)
    }
    /// TASK_INTERACTIVE(): did the bonus beat a nice-dependent threshold?
    fn is_interactive(&self, id: usize) -> bool {
        let nice = self.static_prio[id] - 120;
        let delta = nice * MAX_BONUS / 40 + 2;
        self.prio[id] <= self.static_prio[id] - delta
    }
    /// Starvation guard: stop re-queuing interactive tasks into `active`
    /// if something has been stuck in `expired` too long.
    fn expired_starving(&self, now: u64) -> bool {
        matches!(self.expired_since, Some(t) if now - t >= MAX_SLEEP_AVG_US * self.nr_running)
    }
}

impl Scheduler for O1 {
    fn name(&self) -> &'static str {
        "O(1) (2.6.0, 2003)"
    }
    fn enqueue(&mut self, id: usize, _now: u64, slept: u64) {
        self.sleep_avg[id] = (self.sleep_avg[id] + slept).min(MAX_SLEEP_AVG_US);
        self.prio[id] = self.effective_prio(id);
        self.active.push(self.prio[id] as usize, id, false);
        self.nr_running += 1;
    }
    fn block(&mut self, _id: usize, _now: u64) {
        self.nr_running -= 1;
    }
    fn put_prev(&mut self, id: usize, now: u64) {
        let p = self.prio[id] as usize;
        if !self.slice_done[id] {
            // Preempted mid-slice: it stays at the head of its list.
            self.active.push(p, id, true);
        } else {
            self.slice_done[id] = false;
            if self.is_interactive(id) && !self.expired_starving(now) {
                self.active.push(p, id, false); // the interactivity "reward"
            } else {
                self.expired.push(p, id, false);
                self.expired_since.get_or_insert(now);
            }
        }
    }
    fn pick_next(&mut self, _now: u64) -> Option<usize> {
        if self.active.is_empty() && !self.expired.is_empty() {
            std::mem::swap(&mut self.active, &mut self.expired); // O(1) swap
            self.expired_since = None;
        }
        self.active.pop_first()
    }
    fn account(&mut self, id: usize, ran: u64) {
        self.sleep_avg[id] = self.sleep_avg[id].saturating_sub(ran);
    }
    fn tick(&mut self, id: usize, _now: u64) -> bool {
        self.slice_left[id] -= self.tick_us() as i64;
        if self.slice_left[id] > 0 {
            return false;
        }
        self.prio[id] = self.effective_prio(id);
        self.slice_left[id] = Self::timeslice(self.static_prio[id]);
        self.slice_done[id] = true;
        true
    }
    fn wakeup_preempt(&mut self, curr: usize, woken: usize) -> bool {
        self.prio[woken] < self.prio[curr] // lower number = higher priority
    }
}

// ══════════════════ shared by CFS and EEVDF: nice → weight ═══════════════

/// sched_prio_to_weight[] from kernel/sched/core.c (nice -20 … +19).
/// Each nice step is ~1.25× the CPU share.
const NICE_TO_WEIGHT: [u64; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, //
    9548, 7620, 6100, 4904, 3906, 3121, 2501, 1991, 1586, 1277, //
    1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, //
    110, 87, 70, 56, 45, 36, 29, 23, 18, 15,
];
const NICE_0_WEIGHT: u64 = 1024;

fn weight(nice: i32) -> u64 {
    NICE_TO_WEIGHT[(nice + 20) as usize]
}

/// Real µs → virtual ns: slower for heavy (high-priority) tasks.
fn to_virtual(real_us: u64, w: u64) -> u64 {
    real_us * 1000 * NICE_0_WEIGHT / w
}

// ═════════════════════════ 3. CFS (2.6.23, 2007) ═════════════════════════
//
// Runnable tasks sit in a red-black tree ordered by vruntime (here:
// BTreeSet, also a balanced tree). Always run the leftmost (lowest
// vruntime). Each task's slice = sched_latency × its share of total weight.
// Waking tasks get vruntime ≥ min_vruntime − latency/2 (sleeper credit)
// and preempt if they're ahead by more than wakeup_granularity.
// Constants are the 1-CPU defaults.

const CFS_LATENCY_US: u64 = 6_000;
const CFS_MIN_GRAN_US: u64 = 750;
const CFS_WAKEUP_GRAN_US: u64 = 1_000;
const CFS_NR_LATENCY: usize = 8;

struct Cfs {
    weight: Vec<u64>,
    vruntime: Vec<u64>, // virtual ns
    tree: BTreeSet<(u64, usize)>,
    curr: Option<usize>,
    ran_since_pick: u64,
    min_vruntime: u64,
}

impl Cfs {
    fn new(nices: &[i32]) -> Self {
        Self {
            weight: nices.iter().map(|&n| weight(n)).collect(),
            vruntime: vec![0; nices.len()],
            tree: BTreeSet::new(),
            curr: None,
            ran_since_pick: 0,
            min_vruntime: 0,
        }
    }
    fn update_min_vruntime(&mut self) {
        let curr_v = self.curr.map(|c| self.vruntime[c]);
        let left_v = self.tree.first().map(|&(v, _)| v);
        let lowest = match (curr_v, left_v) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Some(v) = lowest {
            self.min_vruntime = self.min_vruntime.max(v); // never goes backwards
        }
    }
    /// The "dynamic timeslice": period × (my weight / total weight).
    fn sched_slice_us(&self, id: usize) -> u64 {
        let nr = self.tree.len() + self.curr.is_some() as usize;
        let period = if nr > CFS_NR_LATENCY { nr as u64 * CFS_MIN_GRAN_US } else { CFS_LATENCY_US };
        let total: u64 = self.tree.iter().map(|&(_, i)| self.weight[i]).sum::<u64>()
            + self.curr.map_or(0, |c| self.weight[c]);
        period * self.weight[id] / total
    }
}

impl Scheduler for Cfs {
    fn name(&self) -> &'static str {
        "CFS (2.6.23, 2007)"
    }
    fn enqueue(&mut self, id: usize, _now: u64, slept: u64) {
        if slept > 0 {
            // place_entity(): don't let a long sleeper come back with a
            // tiny vruntime and hog the CPU — cap its credit at latency/2.
            let floor = self.min_vruntime.saturating_sub(CFS_LATENCY_US * 1000 / 2);
            self.vruntime[id] = self.vruntime[id].max(floor);
        }
        self.tree.insert((self.vruntime[id], id));
    }
    fn block(&mut self, _id: usize, _now: u64) {
        self.curr = None;
        self.update_min_vruntime();
    }
    fn put_prev(&mut self, id: usize, _now: u64) {
        self.tree.insert((self.vruntime[id], id));
        self.curr = None;
    }
    fn pick_next(&mut self, _now: u64) -> Option<usize> {
        let (_, id) = self.tree.pop_first()?; // leftmost node
        self.curr = Some(id);
        self.ran_since_pick = 0;
        Some(id)
    }
    fn account(&mut self, id: usize, ran: u64) {
        self.vruntime[id] += to_virtual(ran, self.weight[id]);
        self.ran_since_pick += ran;
        self.update_min_vruntime();
    }
    fn tick(&mut self, id: usize, _now: u64) -> bool {
        // check_preempt_tick()
        let ideal = self.sched_slice_us(id);
        if self.ran_since_pick > ideal {
            return true;
        }
        if self.ran_since_pick < CFS_MIN_GRAN_US {
            return false;
        }
        match self.tree.first() {
            Some(&(left_v, _)) => self.vruntime[id] as i64 - left_v as i64 > (ideal * 1000) as i64,
            None => false,
        }
    }
    fn wakeup_preempt(&mut self, curr: usize, woken: usize) -> bool {
        let gran = to_virtual(CFS_WAKEUP_GRAN_US, self.weight[woken]) as i64;
        self.vruntime[curr] as i64 - self.vruntime[woken] as i64 > gran
    }
}

// ═════════════════════════ 4. EEVDF (6.6, 2023) ══════════════════════════
//
// V = weighted average vruntime of the run queue.
// lag = V − vruntime. lag ≥ 0 means "owed CPU time" → eligible.
// Each task requests a fixed slice; deadline = vruntime + slice/weight.
// Pick: among eligible tasks, the earliest virtual deadline.
// Sleepers keep their lag across sleep (clamped), instead of CFS's
// min_vruntime heuristic. RUN_TO_PARITY: the current task finishes its
// slice before a waking task can take over.

const EEVDF_BASE_SLICE_US: u64 = 750;

struct Eevdf {
    weight: Vec<u64>,
    vruntime: Vec<i64>,
    deadline: Vec<i64>,
    vlag: Vec<i64>,
    queue: Vec<usize>, // the kernel uses an augmented rbtree; a Vec is fine for 3 tasks
    curr: Option<usize>,
    resched: bool,
}

impl Eevdf {
    fn new(nices: &[i32]) -> Self {
        let n = nices.len();
        Self {
            weight: nices.iter().map(|&n| weight(n)).collect(),
            vruntime: vec![0; n],
            deadline: vec![0; n],
            vlag: vec![0; n],
            queue: Vec::new(),
            curr: None,
            resched: false,
        }
    }
    fn vslice(&self, id: usize) -> i64 {
        to_virtual(EEVDF_BASE_SLICE_US, self.weight[id]) as i64
    }
    fn members(&self) -> impl Iterator<Item = usize> + '_ {
        self.queue.iter().copied().chain(self.curr)
    }
    fn total_weight(&self) -> i64 {
        self.members().map(|i| self.weight[i] as i64).sum()
    }
    /// V: the weighted average vruntime ("where everyone should be").
    fn avg_vruntime(&self) -> Option<i64> {
        let (mut sw, mut swv) = (0i128, 0i128);
        for i in self.members() {
            sw += self.weight[i] as i128;
            swv += self.weight[i] as i128 * self.vruntime[i] as i128;
        }
        (sw > 0).then(|| (swv / sw) as i64)
    }
}

impl Scheduler for Eevdf {
    fn name(&self) -> &'static str {
        "EEVDF (6.6, 2023)"
    }
    fn enqueue(&mut self, id: usize, _now: u64, slept: u64) {
        if slept == 0 {
            // Brand-new task: start at V with half a slice (PLACE_DEADLINE_INITIAL).
            self.vruntime[id] = self.avg_vruntime().unwrap_or(0);
            self.deadline[id] = self.vruntime[id] + self.vslice(id) / 2;
        } else {
            // place_entity() with PLACE_LAG: restore the lag it had when it
            // slept, scaled so adding it doesn't shift V.
            let v = self.avg_vruntime().unwrap_or(self.vruntime[id]);
            let big_w = self.total_weight();
            let mut lag = self.vlag[id];
            if big_w > 0 {
                lag = lag * (big_w + self.weight[id] as i64) / big_w;
            }
            self.vruntime[id] = v - lag;
            self.deadline[id] = self.vruntime[id] + self.vslice(id);
        }
        self.queue.push(id);
    }
    fn block(&mut self, id: usize, _now: u64) {
        // Remember how far ahead/behind it was, clamped to ±max(2 slices, 1 tick).
        let v = self.avg_vruntime().unwrap_or(self.vruntime[id]);
        let limit = to_virtual((2 * EEVDF_BASE_SLICE_US).max(self.tick_us()), self.weight[id]) as i64;
        self.vlag[id] = (v - self.vruntime[id]).clamp(-limit, limit);
        self.curr = None;
    }
    fn put_prev(&mut self, id: usize, _now: u64) {
        self.queue.push(id);
        self.curr = None;
    }
    fn pick_next(&mut self, _now: u64) -> Option<usize> {
        let v = self.avg_vruntime()?;
        // Eligible (lag ≥ 0) with the earliest deadline...
        let pick = self
            .queue
            .iter()
            .copied()
            .filter(|&i| self.vruntime[i] <= v)
            .min_by_key(|&i| self.deadline[i])
            // ...or, if rounding left nobody eligible, the lowest vruntime.
            .or_else(|| self.queue.iter().copied().min_by_key(|&i| self.vruntime[i]))?;
        self.queue.retain(|&i| i != pick);
        self.curr = Some(pick);
        self.resched = false;
        Some(pick)
    }
    fn account(&mut self, id: usize, ran: u64) {
        self.vruntime[id] += to_virtual(ran, self.weight[id]) as i64;
        // update_deadline(): slice used up → new deadline, ask to reschedule.
        if self.vruntime[id] >= self.deadline[id] {
            self.deadline[id] = self.vruntime[id] + self.vslice(id);
            self.resched = true;
        }
    }
    fn tick(&mut self, _id: usize, _now: u64) -> bool {
        self.resched
    }
    fn wakeup_preempt(&mut self, _curr: usize, _woken: usize) -> bool {
        // RUN_TO_PARITY: curr keeps the CPU until its current slice is used.
        self.resched
    }
}

// ─────────────────────────────── simulator ───────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum State {
    Ready,
    Running,
    Sleeping { until: u64 },
}

struct TaskStats {
    cpu_us: u64,
    /// Every time the task was dispatched: how long it had been waiting.
    waits_us: Vec<u64>,
}

struct RunResult {
    name: &'static str,
    stats: Vec<TaskStats>,
    switches: u64,
    timeline: String,
}

fn simulate(s: &mut dyn Scheduler, specs: &[Spec]) -> RunResult {
    let n = specs.len();
    let mut state = vec![State::Ready; n];
    let mut ready_since = vec![0u64; n];
    let mut sleep_start = vec![0u64; n];
    let mut burst_left: Vec<u64> = specs
        .iter()
        .map(|sp| match sp.kind {
            Kind::Interactive { run_us, .. } => run_us,
            Kind::Hog => 0,
        })
        .collect();
    let mut stats: Vec<TaskStats> = (0..n).map(|_| TaskStats { cpu_us: 0, waits_us: Vec::new() }).collect();
    let mut timeline_counts = vec![vec![0u32; n]; TIMELINE_MS];
    let timeline_end = TIMELINE_START_US + TIMELINE_MS as u64 * 1000;

    let mut curr: Option<usize> = None;
    let mut need_resched = false;
    let mut switches = 0u64;
    let tick = s.tick_us();

    for i in 0..n {
        s.enqueue(i, 0, 0);
    }

    let mut now = 0u64;
    while now < SIM_US {
        // 1. Wake up sleepers whose sleep is over.
        for i in 0..n {
            if let State::Sleeping { until } = state[i] {
                if until <= now {
                    state[i] = State::Ready;
                    ready_since[i] = now;
                    if let Kind::Interactive { run_us, .. } = specs[i].kind {
                        burst_left[i] = run_us;
                    }
                    s.enqueue(i, now, now - sleep_start[i]);
                    if let Some(c) = curr {
                        need_resched |= s.wakeup_preempt(c, i);
                    }
                }
            }
        }

        // 2. Schedule if the CPU is free or a reschedule was requested.
        if curr.is_none() || need_resched {
            need_resched = false;
            let prev = curr.take();
            if let Some(p) = prev {
                s.put_prev(p, now);
                state[p] = State::Ready;
                ready_since[p] = now;
            }
            if let Some(next) = s.pick_next(now) {
                if Some(next) != prev && now >= WARMUP_US {
                    switches += 1;
                    stats[next].waits_us.push(now - ready_since[next]);
                }
                state[next] = State::Running;
                curr = Some(next);
            }
        }

        // 3. Run the current task for one step.
        if let Some(c) = curr {
            if now >= WARMUP_US {
                stats[c].cpu_us += STEP_US;
            }
            if (TIMELINE_START_US..timeline_end).contains(&now) {
                timeline_counts[((now - TIMELINE_START_US) / 1000) as usize][c] += 1;
            }
            s.account(c, STEP_US);
            if let Kind::Interactive { sleep_us, .. } = specs[c].kind {
                burst_left[c] -= STEP_US;
                if burst_left[c] == 0 {
                    let t = now + STEP_US;
                    s.block(c, t);
                    state[c] = State::Sleeping { until: t + sleep_us };
                    sleep_start[c] = t;
                    curr = None;
                }
            }
        }

        now += STEP_US;

        // 4. Timer interrupt.
        if now % tick == 0 {
            if let Some(c) = curr {
                need_resched |= s.tick(c, now);
            }
        }
    }

    // One character per millisecond: whoever ran most in that ms ('.' = idle).
    let timeline = timeline_counts
        .iter()
        .map(|counts| {
            let (best, &max) = counts.iter().enumerate().rev().max_by_key(|&(_, c)| c).unwrap();
            if max == 0 { '.' } else { specs[best].letter }
        })
        .collect();

    RunResult { name: s.name(), stats, switches, timeline }
}

// ──────────────────────────────── output ─────────────────────────────────

fn ms(us: f64) -> String {
    format!("{:.1}", us / 1000.0)
}

fn main() {
    let specs = workload();
    let nices: Vec<i32> = specs.iter().map(|s| s.nice).collect();

    let mut schedulers: Vec<Box<dyn Scheduler>> = vec![
        Box::new(Linux001::new(&nices)),
        Box::new(O1::new(&nices)),
        Box::new(Cfs::new(&nices)),
        Box::new(Eevdf::new(&nices)),
    ];
    let results: Vec<RunResult> = schedulers.iter_mut().map(|s| simulate(s.as_mut(), &specs)).collect();

    let (a, b, c) = (0, 1, 2); // indices into the default workload
    let name_w = 20;

    println!(
        "\nWORKLOAD — 1 CPU, {} s simulated (stats skip the first {} s warm-up)",
        SIM_US / 1_000_000,
        WARMUP_US / 1_000_000
    );
    for s in &specs {
        println!("  {}  {}", s.letter, s.desc);
    }

    // Fairness
    println!("\nFAIRNESS — share of total CPU time");
    println!("{:<name_w$}{:>9}{:>9}{:>9}{:>12}", "scheduler", "A", "B", "C", "A:B ratio");
    for r in &results {
        let pct = |i: usize| 100.0 * r.stats[i].cpu_us as f64 / (SIM_US - WARMUP_US) as f64;
        let ratio = r.stats[a].cpu_us as f64 / r.stats[b].cpu_us.max(1) as f64;
        println!(
            "{:<name_w$}{:>8.1}%{:>8.1}%{:>8.1}%{:>12.2}",
            r.name,
            pct(a),
            pct(b),
            pct(c),
            ratio
        );
    }
    println!(
        "  Target A:B ratio from nice weights: {} / {} = {:.2}",
        weight(specs[a].nice),
        weight(specs[b].nice),
        weight(specs[a].nice) as f64 / weight(specs[b].nice) as f64
    );

    // Responsiveness
    println!("\nRESPONSIVENESS — time waiting before getting the CPU (ms)");
    println!(
        "{:<name_w$}{:>10}{:>10}{:>10}{:>10}{:>12}",
        "scheduler", "C avg", "C worst", "A worst", "B worst", "switches"
    );
    for r in &results {
        let avg = |i: usize| {
            let w = &r.stats[i].waits_us;
            w.iter().sum::<u64>() as f64 / w.len().max(1) as f64
        };
        let worst = |i: usize| *r.stats[i].waits_us.iter().max().unwrap_or(&0) as f64;
        println!(
            "{:<name_w$}{:>10}{:>10}{:>10}{:>10}{:>12}",
            r.name,
            ms(avg(c)),
            ms(worst(c)),
            ms(worst(a)),
            ms(worst(b)),
            r.switches
        );
    }

    // Timeline
    println!(
        "\nTIMELINE — who had the CPU from t = {:.1} s to {:.1} s (1 char = 1 ms)",
        TIMELINE_START_US as f64 / 1e6,
        (TIMELINE_START_US + TIMELINE_MS as u64 * 1000) as f64 / 1e6
    );
    for r in &results {
        println!("{:<name_w$}|{}|", r.name, r.timeline);
    }
    println!();
}
