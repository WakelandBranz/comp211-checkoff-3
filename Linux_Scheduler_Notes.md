https://docs.kernel.org/scheduler/sched-design-CFS.html

### First Scheduler (Weighted Round-Robin, Linus Torvalds)

- In Linux 0.01 the scheduler was very simple
	- Roughly 100 lines in kernel/sched.c
	- Essentially a priority-weighted round-robin that took O(n) time to pick the next task
	- **Fixed task array:** The kernel kept every process in a static array capped at 64 tasks (`NR_TASKS`).
	- **Counter-based selection:** Each task had a `counter`, which was its remaining timeslice. `schedule()` scanned the whole array and ran the runnable task with the highest counter.
	- **Timer decrement:** Each timer tick lowered the running task’s counter by one. When it reached zero, the task was preempted.
	- **Recalculation:** When every runnable task had used up its counter, the kernel gave all tasks new values: `counter = counter/2 + priority`. Sleeping tasks still had counter left over, so they came back with larger values. That gave a natural boost to interactive, I/O-bound processes.

### Second Scheduler (O(1) Scheduler by Ingo Molnar)

**Core data structures**

- **Per-CPU runqueues:** Each CPU had its own runqueue. In 2.4, all CPUs shared one runqueue protected by one lock, which became a bottleneck on SMP machines.
- **Two priority arrays per runqueue:** an **active** array for tasks that still had timeslice left, and an **expired** array for tasks that had used theirs up.
	- Runqueue is a doubly linked list
- **140 priority levels per array:** Levels 0–99 were for real-time tasks. Levels 100–139 were for normal tasks, which is where nice -20 to +19 landed. Each level had its own FIFO list of tasks.
- **A 140-bit bitmap per array:** a bit was set if that priority level had at least one runnable task.

**Picking the next task**

1. Find the first set bit in the active array’s bitmap. The bitmap is always 140 bits, so this takes a fixed number of instructions (about 5 words to check).
2. Run the task at the head of that priority’s list.

The cost is the same with 10 tasks or 10,000.

**When a timeslice runs out**

- The kernel recalculated that task’s priority and timeslice right away, then moved it to the expired array.
- When the active array was empty, the kernel **swapped the active and expired pointers**. That swap is O(1). In 2.4, every task’s timeslice had to be recalculated at the same time, all in one pass.

**Timeslices and dynamic priority**

- **Timeslice length** depended on the nice value: about 800 ms at nice -20, 100 ms at nice 0, and 5 ms at nice +19.
- **Dynamic priority** was the task’s static priority adjusted by up to ±5, based on `sleep_avg`, a measure of how much the task slept. Tasks that slept a lot, such as interactive or I/O-bound ones, got a bonus. CPU-bound tasks got a penalty.
- **Interactive tasks** that finished their timeslice were often put back into the **active** array instead of the expired one. This kept them responsive. A starvation check made sure expired tasks eventually got to run.
### CFS (By Ingo Molnar)

https://docs.kernel.org/scheduler/sched-design-CFS.html

This article is a little too dense for me.

- Merged in 2.6.23
- On real hardware we can only run  a single task at once, so virtual runtimes are important in describing this.

> Virtual runtime is expressed and tracaked via per-task p->se.vruntime (nanosec) value
> 	Accurately timestamps and measures the expected CPU time a task should have gotten
> 		On "ideal" hardware, tasks would execute simultaneously and no task would ever get "out of balance" from the ideal share of CPU time

> [!info] Always tries to run the task with the smallest 'p->se.vruntime' value

- p-> se.vruntime: how much CPU time it has used, in nanoseconds, scaled by its priority weight. It’s the main value the fair scheduler compares to decide who has been treated fairly.

**What each part is**

- **`p`** is a `struct task_struct *`, the kernel’s descriptor for a process or thread.
- **`se`** is a `struct sched_entity` embedded in the task. It holds the fair-scheduler bookkeeping. It’s a separate struct because a sched_entity can also represent a whole **task group** (cgroup), which lets the scheduler split CPU fairly between groups first and then between tasks inside a group.
- **`vruntime`** is a `u64` field in that struct.


https://www.geeksforgeeks.org/operating-systems/completely-fair-scheduler-cfs-and-brain-fuck-scheduler-bfs/

## USEFUL!!!! https://www.youtube.com/watch?v=MkJfuI5_hjc

### Overall CFS Idea

- Each task tracks how much CPU time it’s had (vruntime)
- The scheduler always runs the task that’s had **the least**
- A task stops running when:
    - it has to wait for something, or
    - the timer goes off and it has used up its share
- Then the scheduler picks the next task with the least time

vruntime is the time that the process has been given and ran for weighted by the niceness score

- A lower priority implies time moves at a faster rate compared to that of a high priority task
