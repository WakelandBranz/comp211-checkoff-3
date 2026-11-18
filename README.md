# comp211-checkoff-3

**Wakeland Branz · COMP 211**

**Presentation**
https://youtu.be/_p1z_qz6ZUo

## Scheduler Simulator
> Run ./sched-sim-executable in your terminal (compiled for Linux)!
> If you are trying to run this executable on another operating system, ensure you have cargo installed and recompile using cargo build --release in the sched-sim folder.

Cargo Installation: https://doc.rust-lang.org/cargo/getting-started/installation.html

## Introduction

Every running program wants time on the CPU, but a CPU core can only run one task at a time. The **scheduler** is the part of the operating system that decides which process runs next and for how long. This project looks at how Linux has answered that question over its history, from a ~250-line function written by a college student in 1991 to the scheduler running in the kernel today.

A few terms used throughout:

- **Process:** a running program. The scheduler decides which one gets the CPU.
- **Time slice (quantum):** how long a process can run before it has to give up the CPU.
- **Preemption:** interrupting a running task so it can be resumed later.
- **Context switch:** saving one process's state and loading another's so it can run.
- **Priority / niceness:** a value that decides which processes are favored and how much CPU time they get.

## What I Explored

My original plan was to focus only on **EEVDF**, Linux's current scheduler. I quickly realized it has too many technical details to present well in 5–7 minutes to an audience new to the topic. Instead, I traced how Linux's scheduler evolved through its four major designs:

| Scheduler | Author | Introduced | Core idea |
|---|---|---|---|
| 0.01 scheduler | Linus Torvalds | 1991 (Linux 0.01) | Run the task with the most timeslice left |
| O(1) scheduler | Ingo Molnar | 2002 (kernel 2.5.2) | Constant-time pick using priority arrays and a bitmap |
| Completely Fair Scheduler (CFS) | Ingo Molnar | 2007 (kernel 2.6.23) | Run the task that has had the least CPU time |
| EEVDF | Peter Zijlstra | 2023 (kernel 6.6) | Run the eligible task with the earliest virtual deadline |

For each one, I looked at why it was created, how it works, and what problems eventually led to its replacement.

### 1. The 0.01 Scheduler (1991)

Linus Torvalds wrote Linux 0.01 for his Intel 386 PC while he was a CS student in Helsinki. The scheduler wasn't designed for fairness, multiple CPUs, or many processes. It used a fixed array capped at 64 tasks.

- Each task has a `counter` (its remaining timeslice). `schedule()` scans the whole array and runs the ready task with the highest counter, which takes O(n).
- Each timer tick decrements the running task's counter. At zero, the task is preempted.
- When every ready task is out of time, all tasks are refilled with `counter = counter/2 + priority`.

### 2. The O(1) Scheduler (2002)

By 2002, Linux was running on large servers with many CPUs and thousands of processes, and scanning every task no longer scaled.

- Each CPU gets its own runqueue. In 2.4, one shared runqueue with one lock had become a bottleneck.
- Each runqueue has an **active** and an **expired** array, each with 140 priority levels (0–99 real-time, 100–139 normal) and a 140-bit bitmap marking which levels have runnable tasks.
- To pick a task, it finds the first set bit and runs the head of that priority's list. The cost is the same with 10 tasks or 10,000.
- When a task's timeslice runs out, its next slice is calculated right away and it moves to the expired array. When the active array empties, the two array pointers are simply **swapped**.
- Niceness sets the slice length (about 5 ms at nice +19, 100 ms at nice 0, 800 ms at nice -20). A `sleep_avg` heuristic gives interactive tasks a bonus and CPU hogs a penalty.

### 3. The Completely Fair Scheduler (2007)

O(1)'s interactivity heuristics grew complicated, often guessed wrong, and were easy to fool. Con Kolivas showed that a simpler, fairness-based approach worked better, and CFS followed.

- The goal is to act like an "ideal CPU" that splits time perfectly evenly between all ready programs.
- Each task tracks its **virtual runtime** (`p->se.vruntime`): CPU time used in nanoseconds, scaled by its priority weight.
- The task with the smallest vruntime always runs next. Tasks are kept in a **red-black tree** sorted by vruntime.
- There are no fixed timeslices. Time is divided among everyone who is ready.
- Sleeping tasks fall behind in vruntime, so they naturally run sooner when they wake.

CFS was Linux's default scheduler for about 16 years, the longest of any of its schedulers.

### 4. EEVDF (2023)

CFS was fair over time, but it had no way to express "I need to run *soon*." Years of patches and tuning knobs had piled up to fix responsiveness. EEVDF, based on a 1995 paper by Ion Stoica and Hussein Abdel-Wahab, handles fairness and latency with a single rule.

- It tracks each task's **lag**, meaning how much CPU time it is owed. Tasks that are owed time are **eligible** to run.
- Each task gets a **virtual deadline** based on its slice length.
- The eligible task with the **earliest deadline** runs next.
- Shorter slices mean earlier deadlines, so a task gets quicker turns with the same total share.
- It still uses a red-black tree but needs far fewer special cases than CFS.

## Most Interesting Discoveries

- **The first scheduler already favored interactive programs, by accident.** In the 0.01 refill formula `counter/2 + priority`, tasks that were sleeping still had time left over, so they came back with larger counters. Programs waiting on input got a boost without any special interactivity code.
- **O(1) got constant time from a bitmap and a pointer swap.** Because the bitmap is always 140 bits, finding the highest-priority task takes a fixed number of instructions. Swapping the active and expired arrays replaced the old pass that recalculated every task's timeslice.
- **A faster algorithm isn't automatically a better scheduler.** O(1) had excellent time complexity, but its guesses about which tasks were "interactive" were its weak point. CFS replaced the guessing with a simple fairness rule and ended up more responsive.
- **In CFS, time moves at different speeds.** vruntime is weighted by niceness, so a low-priority task's virtual clock runs faster than a high-priority task's. The scheduler just compares one number, and the weighting handles priority automatically.
- **`se` isn't just a task.** `p->se` is a `struct sched_entity`, which is kept separate from the task because it can also represent a whole task group (cgroup). This lets the scheduler split CPU fairly between groups first, then between the tasks inside each group.
- **The current scheduler comes from a 28-year-old paper.** EEVDF was published in 1995 but didn't replace CFS until 2023, and it solved CFS's latency problems with one rule instead of more tuning knobs.

## Resources

- [Linux kernel docs: CFS Scheduler](https://docs.kernel.org/scheduler/sched-design-CFS.html)
- [GeeksforGeeks: Completely Fair Scheduler (CFS) and Brain Fuck Scheduler (BFS)](https://www.geeksforgeeks.org/operating-systems/completely-fair-scheduler-cfs-and-brain-fuck-scheduler-bfs/)
- [YouTube: CFS explainer](https://www.youtube.com/watch?v=MkJfuI5_hjc)
