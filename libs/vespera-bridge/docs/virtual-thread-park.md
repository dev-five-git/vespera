# Virtual threads: park vs. pin vs. poll-first (2026-11-13)

On a virtual thread the bridge must not block inside a native frame for a
whole handler run: a virtual thread in native code is **pinned** to its
carrier, and a handful of slow handlers then starve every other virtual
thread. Until 0.6 the bridge (`ParkingDispatch`, used by the proxy's SYNC
path, the DIRECT heap fallback and `VesperaClient.send()`) therefore sent
every virtual-thread request through `dispatchAsync` + `join()`. That costs
two cross-thread wake-ups per request (Java → Tokio worker, Tokio worker →
ForkJoinPool carrier) even when the handler never waits.

Since 0.6 the default is **poll-first** (`AsyncDispatch.dispatchPollFirst0`):
the dispatch future is polled once on the calling thread inside the shared
runtime's context. Ready → the wire comes straight back (the carrier did
only CPU work, like any Java code on a virtual thread). Pending → the
half-polled future moves to a Tokio worker that completes a
`CompletableFuture`, and the virtual thread parks on it — the carrier is
never held across a wait. `-Dvespera.virtualThread.pollFirst=false` restores
the old always-park behaviour. Task context: the first poll runs outside any
Tokio task (`tokio::task::try_id()` is `None` until the first wait, then the
spawned task's id; the handler may resume on another thread; `task_local!`
scopes are unaffected) — opt out with the same property when a handler needs
one stable task context (`crates/vespera_jni/src/jni_impl_park.rs` module docs).

## Method

`bench/VirtualThreadParkBench.java` (single file, not a CI gate), run on the
release `rust-jni-demo` cdylib:

```bash
cargo build -p rust-jni-demo --lib --release
(cd libs/vespera-bridge && ./gradlew jar)
javac -cp vespera-bridge-<v>.jar -d out libs/vespera-bridge/bench/VirtualThreadParkBench.java
java -Xms1g -Xmx1g -XX:+UseParallelGC -cp "vespera-bridge-<v>.jar:out" \
     -Djava.library.path=target/release \
     -Dbench.cellMillis=1000 -Dbench.rounds=5 VirtualThreadParkBench
```

- N virtual threads (1 / 64 / 1000) loop the full caller path (encode,
  cross JNI, decode the status) for 1 s per cell after a 200 ms ramp-up;
  every op's wall latency is recorded. Cells run round-robin, mode order
  rotated per round, 5 rounds; the table shows the **median of the 5
  per-round** p50 / p99 / throughput values.
- Modes: `pinned` = `dispatchBytes`; `direct` = raw `dispatchDirect` from a
  shared direct-buffer pool (also pinned); `park` = `dispatch(wire).join()`
  (pre-0.6 behaviour); `pollfirst` = `ParkingDispatch.park` (0.6 default);
  `hybrid` = a rejected prototype that pins while fewer than 12 (= CPUs/2)
  calls are pinned and parks otherwise.
- Workloads: `GET /health` (completes without waiting) and
  `GET /diag/sleep/vtbench/1` (`tokio::time::sleep(1 ms)` — a stand-in for
  I/O; Windows' ~15.6 ms timer granularity turns it into ~15.6 ms).
- "bystander": a platform thread starts a trivial virtual thread about every
  millisecond and records how long it waits for a carrier (p99).
  `starved` = not a single probe ran during the measured second.

Machine: Windows 11, 24 logical CPUs, Temurin 21.0.13, default virtual-thread
scheduler (24 carriers).

## Results

| workload | vthreads | mode | p50 µs | p99 µs | ops/s | bystander vthread start p99 µs |
|---|---:|---|---:|---:|---:|---:|
| `/health` | 1 | pinned | 1.4 | 2.2 | 698,976 | 91 |
| `/health` | 1 | park | 15.5 | 41.9 | 52,596 | 49 |
| `/health` | 1 | pollfirst | 1.3 | 2.1 | 712,300 | 86 |
| `/health` | 1 | direct | 1.3 | 2.1 | 712,447 | 82 |
| `/health` | 1 | hybrid | 1.4 | 2.2 | 697,053 | 81 |
| `/health` | 64 | pinned | 3.1 | 6.5 | 5,868,749 | starved |
| `/health` | 64 | park | 40.6 | 229.6 | 1,215,712 | 142 |
| `/health` | 64 | pollfirst | 3.1 | 5.0 | 6,113,863 | starved |
| `/health` | 64 | direct | 3.8 | 7.3 | 4,904,984 | starved |
| `/health` | 64 | hybrid | 4.0 | 94.4 | 3,481,565 | 1078 |
| `/health` | 1000 | pinned | 3.1 | 6.2 | 5,817,604 | starved |
| `/health` | 1000 | park | 619.4 | 3008.8 | 1,255,000 | 3613 |
| `/health` | 1000 | pollfirst | 3.1 | 4.8 | 6,143,981 | starved |
| `/health` | 1000 | direct | 3.8 | 7.3 | 4,929,140 | starved |
| `/health` | 1000 | hybrid | 4.2 | 2441.6 | 3,112,213 | 5287 |
| `/diag/sleep/vtbench/1` | 1 | pinned | 15620.3 | 17456.3 | 74 | 82 |
| `/diag/sleep/vtbench/1` | 1 | park | 15620.0 | 17276.9 | 73 | 80 |
| `/diag/sleep/vtbench/1` | 1 | pollfirst | 15644.6 | 17879.7 | 72 | 109 |
| `/diag/sleep/vtbench/1` | 1 | direct | 15672.2 | 17641.8 | 74 | 83 |
| `/diag/sleep/vtbench/1` | 1 | hybrid | 15632.5 | 17438.0 | 75 | 71 |
| `/diag/sleep/vtbench/1` | 64 | pinned | 15614.9 | 17278.2 | 1,750 | starved |
| `/diag/sleep/vtbench/1` | 64 | park | 15730.1 | 17409.5 | 4,667 | 106 |
| `/diag/sleep/vtbench/1` | 64 | pollfirst | 15665.0 | 17227.6 | 4,514 | 97 |
| `/diag/sleep/vtbench/1` | 64 | direct | 15599.2 | 17365.2 | 1,726 | starved |
| `/diag/sleep/vtbench/1` | 64 | hybrid | 15661.5 | 17792.3 | 4,547 | 77 |
| `/diag/sleep/vtbench/1` | 1000 | pinned | 15632.6 | 17516.0 | 1,728 | starved |
| `/diag/sleep/vtbench/1` | 1000 | park | 15606.5 | 17631.8 | 64,052 | 67 |
| `/diag/sleep/vtbench/1` | 1000 | pollfirst | 15544.8 | 17756.8 | 71,458 | 86 |
| `/diag/sleep/vtbench/1` | 1000 | direct | 15631.1 | 17134.9 | 1,756 | starved |
| `/diag/sleep/vtbench/1` | 1000 | hybrid | 15701.1 | 18888.1 | 67,592 | 228 |

## Reading it

- **`park` is expensive when the handler does not wait**: 15.5 µs p50 vs
  1.4 µs pinned for a single caller (~11x), and at 64-1000 virtual threads
  it tops out at ~1.2M ops/s vs ~5.8M pinned — the two wake-ups per request
  dominate; with 1000 threads its p99 reaches 3 ms of queueing.
- **Pinning is catastrophic when the handler waits**: with 64+ callers
  `pinned` / `direct` are capped at carriers / handler time (~1.7k ops/s =
  24 / 15.6 ms) and the bystander probe never gets a carrier (`starved`),
  while `park` reaches 64k ops/s with 1000 threads and the probe starts in
  ~0.1 ms.
- **`pollfirst` gets both**: pinned-call cost (1.3 µs p50, ~6.1M ops/s) for
  the ready handler and park-level throughput (71k ops/s at 1000 threads,
  bystander ~0.1 ms) for the waiting one.
- The `starved` bystander on the `/health` rows of `pinned` / `pollfirst` /
  `direct` is a property of the benchmark, not of JNI: 64+ virtual threads
  in a tight loop that never blocks never yield their carriers (virtual
  threads are not preempted). A real server parks on socket I/O between
  requests. The same cells under `park` show what the extra parking buys:
  scheduling fairness at ~5x lower throughput.
- `hybrid` (pin up to CPUs/2 at a time, park beyond) loses to `pollfirst`
  everywhere: the parked overflow drags p99 to 0.1-2.4 ms on the fast
  workload and it still pins up to 12 carriers across waits.

Noise: single machine, desktop OS, one JVM. Short exploratory runs (300 ms
cells, one round) moved individual cells by up to 2-3x (e.g. single-caller
`pinned` 259k vs 699k ops/s), so only the median-of-5 table above is meant
for comparison, and only differences well beyond ~20% should be read into it.
Windows timer granularity makes the sleep workload's latency ~15.6 ms
instead of 1 ms — compare its rows by throughput and bystander, not latency.
