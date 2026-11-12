import com.devfive.vespera.bridge.VesperaBridge;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Virtual-thread dispatch benchmark (JDK 21+): what does parking a virtual
 * thread on {@code dispatchAsync} + {@code join()} (the bridge's
 * {@code ParkingDispatch}) cost compared with a plain pinned native call?
 *
 * <p>Not a CI gate; run by hand (see {@code docs/virtual-thread-park.md}):
 * <pre>
 *   javac -cp vespera-bridge.jar -d out bench/VirtualThreadParkBench.java
 *   java -cp "vespera-bridge.jar;out" -Djava.library.path=&lt;dir of rust_jni_demo&gt; \
 *        VirtualThreadParkBench
 * </pre>
 *
 * <p>For every (workload, concurrency, mode) cell, {@code N} virtual threads
 * loop the full caller path (encode, cross JNI, decode the status) for
 * {@code bench.cellMillis}; each op's wall latency is recorded.  Cells run
 * round-robin for {@code bench.rounds} rounds so machine drift hits every
 * mode alike; per cell the MEDIAN of the per-round p50 / p99 / throughput is
 * reported.  A "bystander" platform thread runs alongside every cell,
 * starting a trivial virtual thread about every millisecond: its start
 * latency (p99) shows how starved the carrier pool is — the actual damage pinning does to the
 * rest of a virtual-thread application.
 *
 * <p>Modes: {@code pinned} = {@code dispatchBytes} (carrier pinned for the
 * whole handler), {@code park} = {@code dispatch(wire).join()} (the bridge on virtual threads
 * before 0.6), {@code pollfirst} = {@code ParkingDispatch.park} (current: poll once
 * inline, park only when pending), {@code hybrid} = pin while fewer than
 * {@code bench.hybridPinned} calls are pinned, else {@code park}, {@code direct} = raw
 * {@code dispatchDirect} from a shared pool of direct buffers (pinned).
 */
public final class VirtualThreadParkBench {

    private static final Map<String, String> GET_HEADERS = Map.of("accept", "text/plain");

    @FunctionalInterface
    private interface Op {
        int run(String path) throws Throwable;
    }

    private static int pinned(String path) {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", path, null, GET_HEADERS, null);
        return VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire)).status();
    }

    private static int park(String path) {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", path, null, GET_HEADERS, null);
        return VesperaBridge.decodeResponse(VesperaBridge.dispatch(wire).join()).status();
    }

    /** {@code ParkingDispatch.park} (package-private; poll-first unless -Dvespera.virtualThread.pollFirst=false). */
    private static final java.lang.invoke.MethodHandle POLL_FIRST = pollFirstHandle();

    private static java.lang.invoke.MethodHandle pollFirstHandle() {
        try {
            java.lang.reflect.Method m = Class.forName("com.devfive.vespera.bridge.ParkingDispatch")
                    .getDeclaredMethod("park", byte[].class);
            m.setAccessible(true);
            return java.lang.invoke.MethodHandles.lookup().unreflect(m);
        } catch (ReflectiveOperationException e) {
            return null;
        }
    }

    private static int pollFirst(String path) throws Throwable {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", path, null, GET_HEADERS, null);
        return VesperaBridge.decodeResponse((byte[]) POLL_FIRST.invokeExact(wire)).status();
    }

    private static final ConcurrentLinkedQueue<ByteBuffer[]> DIRECT_POOL = new ConcurrentLinkedQueue<>();

    private static int direct(String path) {
        ByteBuffer[] bufs = DIRECT_POOL.poll();
        if (bufs == null) {
            bufs = new ByteBuffer[] {ByteBuffer.allocateDirect(4096), ByteBuffer.allocateDirect(64 * 1024)};
        }
        try {
            int inLen = VesperaBridge.encodeRequestInto(null, "GET", path, null, GET_HEADERS, null, bufs[0]);
            int n = VesperaBridge.dispatchDirect(bufs[0], inLen, bufs[1]);
            if (n < 0) {
                throw new IllegalStateException("direct response overflow: " + n);
            }
            byte[] wire = new byte[n];
            bufs[1].get(0, wire, 0, n);
            return VesperaBridge.decodeResponse(wire).status();
        } finally {
            DIRECT_POOL.offer(bufs);
        }
    }

    /** Prototype hybrid: pin while fewer than {@code bench.hybridPinned} dispatches are pinned, else park. */
    private static final Semaphore PIN_SLOTS = new Semaphore((int) longProp(
            "bench.hybridPinned", Math.max(1, Runtime.getRuntime().availableProcessors() / 2)));

    private static int hybrid(String path) throws Throwable {
        if (PIN_SLOTS.tryAcquire()) {
            try {
                return pinned(path);
            } finally {
                PIN_SLOTS.release();
            }
        }
        return park(path);
    }

    // ── harness ──────────────────────────────────────────────────────────

    private static long longProp(String key, long def) {
        String v = System.getProperty(key);
        return v == null || v.isBlank() ? def : Long.parseLong(v.trim());
    }

    private static final class Samples {
        long[] ns = new long[1024];
        int n;

        void add(long v) {
            if (n == ns.length) {
                ns = Arrays.copyOf(ns, n * 2);
            }
            ns[n++] = v;
        }
    }

    /** One cell run: {p50 ns, p99 ns, ops/s, bystander virtual-thread start p99 ns}. */
    private static double[] runCell(Op op, String path, int threads, long millis) throws Exception {
        AtomicBoolean stop = new AtomicBoolean();
        AtomicBoolean measuring = new AtomicBoolean();
        Samples[] perThread = new Samples[threads];
        CountDownLatch done = new CountDownLatch(threads);
        long[] failures = new long[1];
        List<Thread> started = new ArrayList<>(threads);
        for (int t = 0; t < threads; t++) {
            Samples s = perThread[t] = new Samples();
            started.add(Thread.ofVirtual().start(() -> {
                try {
                    while (!stop.get()) {
                        long t0 = System.nanoTime();
                        int status = op.run(path);
                        long dt = System.nanoTime() - t0;
                        if (status != 200) {
                            synchronized (failures) {
                                failures[0]++;
                            }
                        }
                        if (measuring.get()) {
                            s.add(dt);
                        }
                    }
                } catch (Throwable e) {
                    synchronized (failures) {
                        failures[0]++;
                    }
                } finally {
                    done.countDown();
                }
            }));
        }
        Samples bystander = new Samples();
        // Platform probe thread: start a trivial virtual thread about every
        // millisecond and record how long it waits for a carrier.
        Thread by = Thread.ofPlatform().daemon().start(() -> {
            long[] ranAt = new long[1];
            while (!stop.get()) {
                long t0 = System.nanoTime();
                Thread probe = Thread.ofVirtual().start(() -> ranAt[0] = System.nanoTime());
                try {
                    probe.join();
                } catch (InterruptedException e) {
                    return;
                }
                if (measuring.get()) {
                    bystander.add(ranAt[0] - t0);
                }
                java.util.concurrent.locks.LockSupport.parkNanos(1_000_000L);
            }
        });
        // ramp-up: a fifth of the cell, excluded from the samples
        Thread.sleep(Math.max(20, millis / 5));
        measuring.set(true);
        long t0 = System.nanoTime();
        Thread.sleep(millis);
        measuring.set(false);
        long elapsed = System.nanoTime() - t0;
        stop.set(true);
        if (!done.await(60, TimeUnit.SECONDS)) {
            throw new IllegalStateException("workers did not stop");
        }
        by.join();
        if (failures[0] > 0) {
            throw new IllegalStateException(failures[0] + " failed ops");
        }
        int total = 0;
        for (Samples s : perThread) {
            total += s.n;
        }
        long[] all = new long[total];
        int k = 0;
        for (Samples s : perThread) {
            System.arraycopy(s.ns, 0, all, k, s.n);
            k += s.n;
        }
        Arrays.sort(all);
        long[] b = Arrays.copyOf(bystander.ns, bystander.n);
        Arrays.sort(b);
        return new double[] {
            pct(all, 0.50), pct(all, 0.99), total * 1e9 / elapsed, b.length == 0 ? Double.POSITIVE_INFINITY : pct(b, 0.99),
        };
    }

    private static double pct(long[] sorted, double q) {
        if (sorted.length == 0) {
            return Double.NaN;
        }
        return sorted[Math.min(sorted.length - 1, (int) (q * sorted.length))];
    }

    private static double median(double[] v) {
        double[] s = v.clone();
        Arrays.sort(s);
        return s[s.length / 2];
    }

    public static void main(String[] args) throws Exception {
        long cellMillis = longProp("bench.cellMillis", 1000);
        int rounds = (int) longProp("bench.rounds", 5);
        String[] modes = System.getProperty("bench.modes", "pinned,park,pollfirst,direct,hybrid").split(",");
        String[] workloads = System.getProperty("bench.workloads", "/health,/diag/sleep/vtbench/1").split(",");
        int[] concurrency = Arrays.stream(System.getProperty("bench.threads", "1,64,1000").split(","))
                .mapToInt(s -> Integer.parseInt(s.trim())).toArray();

        VesperaBridge.init(System.getProperty("bench.library", "rust_jni_demo"));
        System.err.printf("JDK %s, %d CPUs, carriers=%s, hybridPinned=%d%n",
                System.getProperty("java.version"), Runtime.getRuntime().availableProcessors(),
                System.getProperty("jdk.virtualThreadScheduler.parallelism", "default"),
                PIN_SLOTS.availablePermits());

        Op[] ops = new Op[modes.length];
        for (int m = 0; m < modes.length; m++) {
            ops[m] = switch (modes[m].trim()) {
                case "pinned" -> VirtualThreadParkBench::pinned;
                case "park" -> VirtualThreadParkBench::park;
                case "pollfirst" -> VirtualThreadParkBench::pollFirst;
                case "direct" -> VirtualThreadParkBench::direct;
                case "hybrid" -> VirtualThreadParkBench::hybrid;
                default -> throw new IllegalArgumentException("mode " + modes[m]);
            };
        }

        // JIT warm-up of every mode on every workload.
        for (String w : workloads) {
            for (Op op : ops) {
                runCell(op, w, 16, 500);
            }
        }

        double[][][][][] res = new double[workloads.length][concurrency.length][modes.length][4][rounds];
        for (int r = 0; r < rounds; r++) {
            for (int w = 0; w < workloads.length; w++) {
                for (int c = 0; c < concurrency.length; c++) {
                    for (int mm = 0; mm < modes.length; mm++) {
                        // rotate the mode order per round so no mode always runs first
                        int m = (mm + r) % modes.length;
                        double[] cell = runCell(ops[m], workloads[w], concurrency[c], cellMillis);
                        for (int i = 0; i < 4; i++) {
                            res[w][c][m][i][r] = cell[i];
                        }
                    }
                }
            }
            System.err.println("round " + (r + 1) + "/" + rounds + " done");
        }

        System.out.println("| workload | vthreads | mode | p50 µs | p99 µs | ops/s | bystander vthread start p99 µs |");
        System.out.println("|---|---:|---|---:|---:|---:|---:|");
        for (int w = 0; w < workloads.length; w++) {
            for (int c = 0; c < concurrency.length; c++) {
                for (int m = 0; m < modes.length; m++) {
                    double[][] x = res[w][c][m];
                    System.out.println(String.format(Locale.ROOT, "| `%s` | %d | %s | %.1f | %.1f | %,.0f | %s |",
                            workloads[w], concurrency[c], modes[m].trim(),
                            median(x[0]) / 1e3, median(x[1]) / 1e3, median(x[2]),
                            Double.isInfinite(median(x[3])) ? "starved" : String.format(Locale.ROOT, "%.0f", median(x[3]) / 1e3)));
                }
            }
        }
        VesperaBridge.shutdown(java.time.Duration.ofSeconds(5));
    }

    private VirtualThreadParkBench() {}
}
