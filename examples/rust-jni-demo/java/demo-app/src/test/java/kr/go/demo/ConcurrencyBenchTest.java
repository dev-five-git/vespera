package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.Map;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;

/** E2E JNI concurrency throughput benchmark gated behind {@code -Dvespera.bench=true}. */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class ConcurrencyBenchTest {

    private static final int[] THREAD_COUNTS = {1, 2, 4, 8, 16};
    private static final int WARMUP_SECONDS = 1;
    private static final int MEASURE_SECONDS = 3;
    private static final Map<String, String> HEADERS = Map.of("accept", "application/json");

    @BeforeAll
    static void setUp() {
        VesperaBridge.init("rust_jni_demo");
    }

    // Mode implementations: intentionally equivalent to SmallRequestLatencyBenchTest
    // so latency, allocation, and concurrency numbers describe the same code path.
    private static int syncOnce() {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, HEADERS, null);
        return VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire)).status();
    }

    private static int directOnce() {
        ByteBuffer resp =
                VesperaBridge.dispatchDirectPooled(null, "GET", "/health", null, HEADERS, null, true);
        // Consume like the controller does: header region must be parsed.
        byte[] out = new byte[resp.remaining()];
        resp.get(out);
        return VesperaBridge.decodeResponse(out).status();
    }

    private interface Op {
        int run() throws IOException;
    }

    private record Result(long totalOps, double opsPerSecond) {}

    private static Result measureConcurrency(String mode, Op op, int threads) throws Exception {
        CountDownLatch ready = new CountDownLatch(threads);
        CountDownLatch start = new CountDownLatch(1);
        CountDownLatch done = new CountDownLatch(threads);
        AtomicReference<Throwable> failure = new AtomicReference<>();
        long[] counts = new long[threads];

        for (int i = 0; i < threads; i++) {
            int threadIndex = i;
            Thread worker =
                    new Thread(
                            () -> {
                                try {
                                    ready.countDown();
                                    start.await();

                                    long warmupUntil =
                                            System.nanoTime()
                                                    + TimeUnit.SECONDS.toNanos(WARMUP_SECONDS);
                                    while (System.nanoTime() < warmupUntil) {
                                        if (op.run() != 200) {
                                            throw new IllegalStateException(mode + " warmup non-200");
                                        }
                                    }

                                    long measured = 0;
                                    long measureUntil =
                                            System.nanoTime()
                                                    + TimeUnit.SECONDS.toNanos(MEASURE_SECONDS);
                                    while (System.nanoTime() < measureUntil) {
                                        if (op.run() != 200) {
                                            throw new IllegalStateException(mode + " measure non-200");
                                        }
                                        measured++;
                                    }
                                    counts[threadIndex] = measured;
                                } catch (Throwable t) {
                                    failure.compareAndSet(null, t);
                                } finally {
                                    done.countDown();
                                }
                            },
                            "vespera-conc-" + mode + "-" + threads + "-" + i);
            worker.start();
        }

        if (!ready.await(30, TimeUnit.SECONDS)) {
            throw new AssertionError(mode + " workers did not become ready");
        }
        start.countDown();
        long timeout = WARMUP_SECONDS + MEASURE_SECONDS + 30L;
        if (!done.await(timeout, TimeUnit.SECONDS)) {
            throw new AssertionError(mode + " workers did not finish within timeout");
        }

        Throwable t = failure.get();
        if (t instanceof Exception) {
            throw (Exception) t;
        }
        if (t instanceof Error) {
            throw (Error) t;
        }
        if (t != null) {
            throw new RuntimeException(t);
        }

        long totalOps = 0;
        for (long count : counts) {
            totalOps += count;
        }
        double opsPerSecond = totalOps / (double) MEASURE_SECONDS;
        return new Result(totalOps, opsPerSecond);
    }

    private static void measureMode(String mode, Op op) throws Exception {
        double baseline = 0.0;
        for (int threads : THREAD_COUNTS) {
            Result result = measureConcurrency(mode, op, threads);
            if (threads == 1) {
                baseline = result.opsPerSecond();
            }
            double scalingEfficiency = result.opsPerSecond() / (threads * baseline) * 100.0;
            System.out.printf(
                    "VESPERA_CONC %s threads=%d ops_per_sec=%.0f scaling_eff=%.1f total_ops=%d%n",
                    mode, threads, result.opsPerSecond(), scalingEfficiency, result.totalOps());
        }
    }

    @Test
    void concurrencyThroughputByMode() throws Exception {
        int logicalCpus = Runtime.getRuntime().availableProcessors();
        System.out.printf(
                "VESPERA_CONC cpus logical=%d warmup_seconds=%d measure_seconds=%d%n",
                logicalCpus, WARMUP_SECONDS, MEASURE_SECONDS);
        measureMode("sync_dispatch_bytes", ConcurrencyBenchTest::syncOnce);
        measureMode("direct_pooled", ConcurrencyBenchTest::directOnce);
    }
}
