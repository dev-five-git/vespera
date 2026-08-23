package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import com.sun.management.ThreadMXBean;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.lang.management.ManagementFactory;
import java.nio.ByteBuffer;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;

/**
 * E2E JNI <strong>allocation</strong> benchmark gated behind
 * {@code -Dvespera.bench=true} — companion to {@link SmallRequestLatencyBenchTest}.
 *
 * <p>Measures <strong>JVM bytes allocated per dispatch</strong> on the calling
 * thread for each of the five dispatch modes, using
 * {@link com.sun.management.ThreadMXBean#getThreadAllocatedBytes(long)}.
 * Quantifies the memory dimension that the recently-landed streaming
 * chunk-buffer TLS pooling targets.
 *
 * <h2>Why calling-thread measurement captures the pooling win</h2>
 *
 * <p>The streaming JNI entries
 * ({@code Java_..._dispatchFullStreamingWithHeader} and friends in
 * {@code crates/vespera_jni/src/jni_impl.rs}) allocate the Java byte[] chunk
 * buffers via {@code env.new_byte_array(...)} on the <em>JNI entry thread</em>
 * — i.e. the calling thread. Same for {@code set_region} / {@code get_region}
 * on those arrays. Before TLS pooling those landed as fresh JVM allocations
 * per dispatch; after pooling the same {@code GlobalRef<JByteArray>} is
 * reused across calls, so the calling-thread allocation count drops to
 * effectively the request/response wire bytes plus a few small Java objects.
 *
 * <h2>Async caveat (honest)</h2>
 *
 * <p>For {@code async_completable_future}, the {@code CompletableFuture}
 * completion happens on a Rust Tokio worker thread (a daemon-attached
 * cached worker), not the calling thread. This measurement therefore
 * captures only what the <em>caller pays</em>: encoding the request,
 * constructing the future, and {@code future.get()}-side allocations.
 * Completion-side allocations on the daemon thread are not visible here
 * and would require per-thread {@code getThreadAllocatedBytes} on the
 * worker, which we don't observe by design.
 *
 * <h2>Protocol</h2>
 *
 * <ul>
 *   <li>{@code WARMUP=5_000} iterations to stabilize JIT / inlining /
 *       TLS-pool fill.
 *   <li>{@code MEASURE=20_000} iterations; bytes/op =
 *       {@code (allocAfter - allocBefore) / MEASURE}.
 *   <li>Single-threaded loop, pinned to one calling thread.
 *   <li>Loop body keeps no per-iteration objects in Java besides what the
 *       dispatch helpers themselves create — the measurement-harness's own
 *       per-op allocation is intentionally zero (a {@code long} blackhole
 *       accumulator only).
 * </ul>
 *
 * <h2>Output</h2>
 *
 * <p>One line per mode (parseable, same style as {@code VESPERA_BENCH}):
 * <pre>VESPERA_ALLOC &lt;mode&gt; bytes_per_op=&lt;N&gt;</pre>
 *
 * <p>Assertion: weak sanity only ({@code bytes_per_op &gt;= 0}). This is a
 * measurement tool, not a pass/fail gate — exact numbers are
 * machine/JDK-dependent.
 */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class AllocationBenchTest {

    private static final int WARMUP = 5_000;
    private static final int MEASURE = 20_000;
    private static final Map<String, String> HEADERS = Map.of("accept", "application/json");

    @BeforeAll
    static void setUp() {
        VesperaBridge.init("rust_jni_demo");
    }

    private static final class CountingOutputStream extends OutputStream {
        long count;

        @Override
        public void write(int b) {
            count++;
        }

        @Override
        public void write(byte[] b, int off, int len) {
            count += len;
        }
    }

    // --- Mode implementations: kept byte-for-byte equivalent to
    //     SmallRequestLatencyBenchTest so the latency and allocation
    //     numbers describe the same code path. ---

    private static int syncOnce() {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, HEADERS, null);
        return VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire)).status();
    }

    private static int directOnce() {
        ByteBuffer resp =
                VesperaBridge.dispatchDirectPooled(null, "GET", "/health", null, HEADERS, null, true);
        byte[] out = new byte[resp.remaining()];
        resp.get(out);
        return VesperaBridge.decodeResponse(out).status();
    }

    private static int streamingOnce() throws IOException {
        byte[] wireHeader = VesperaBridge.encodeRequestHeader("GET", "/health", null, HEADERS);
        CountingOutputStream sink = new CountingOutputStream();
        int[] status = new int[1];
        VesperaBridge.dispatchFullStreamingWithHeader(
                wireHeader,
                headerBytes -> status[0] = VesperaBridge.decodeResponse(headerBytes).status(),
                new ByteArrayInputStream(new byte[0]),
                sink);
        return status[0];
    }

    private static int asyncOnce() {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, HEADERS, null);
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        VesperaBridge.dispatchAsync(future, wire);
        try {
            byte[] resp = future.get(30, TimeUnit.SECONDS);
            return VesperaBridge.decodeResponse(resp).status();
        } catch (InterruptedException | ExecutionException | TimeoutException e) {
            throw new RuntimeException(e);
        }
    }

    private static int responseStreamingOnce() {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, HEADERS, null);
        CountingOutputStream sink = new CountingOutputStream();
        int[] status = new int[1];
        VesperaBridge.dispatchStreamingWithHeader(
                wire,
                headerBytes -> status[0] = VesperaBridge.decodeResponse(headerBytes).status(),
                sink);
        return status[0];
    }

    private interface Op {
        int run() throws IOException;
    }

    /**
     * Measure bytes allocated by the calling thread across MEASURE
     * iterations. Returns bytes/op (integer). The loop body contains no
     * Java allocations besides the {@code long} blackhole and what the
     * dispatch helpers themselves do — so the per-op number describes the
     * dispatch path's calling-thread allocation footprint.
     */
    private static long measureAlloc(String mode, Op op, ThreadMXBean tmx) throws IOException {
        long tid = Thread.currentThread().getId();

        // Warmup — let JIT settle, TLS pools fill, classes load.
        for (int i = 0; i < WARMUP; i++) {
            if (op.run() != 200) {
                throw new IllegalStateException(mode + " warmup non-200");
            }
        }

        long blackhole = 0;
        long allocBefore = tmx.getThreadAllocatedBytes(tid);
        for (int i = 0; i < MEASURE; i++) {
            blackhole += op.run();
        }
        long allocAfter = tmx.getThreadAllocatedBytes(tid);

        long delta = allocAfter - allocBefore;
        long bytesPerOp = delta / MEASURE;

        System.out.printf(
                "VESPERA_ALLOC %s bytes_per_op=%d (total_delta=%d iters=%d blackhole=%d)%n",
                mode, bytesPerOp, delta, MEASURE, blackhole);

        if (bytesPerOp < 0) {
            throw new AssertionError(
                    mode + " bytes_per_op<0 (delta=" + delta + " iters=" + MEASURE + ")");
        }
        return bytesPerOp;
    }

    @Test
    void allocationPerDispatchByMode() throws IOException {
        java.lang.management.ThreadMXBean base = ManagementFactory.getThreadMXBean();
        Assumptions.assumeTrue(
                base instanceof ThreadMXBean,
                "platform ThreadMXBean is not com.sun.management.ThreadMXBean — non-HotSpot JVM?");
        ThreadMXBean tmx = (ThreadMXBean) base;
        Assumptions.assumeTrue(
                tmx.isThreadAllocatedMemorySupported(),
                "ThreadMXBean.isThreadAllocatedMemorySupported()==false on this JVM");
        if (!tmx.isThreadAllocatedMemoryEnabled()) {
            tmx.setThreadAllocatedMemoryEnabled(true);
        }

        long sync = measureAlloc("sync_dispatch_bytes", AllocationBenchTest::syncOnce, tmx);
        long direct = measureAlloc("direct_pooled", AllocationBenchTest::directOnce, tmx);
        long respStreaming =
                measureAlloc(
                        "response_streaming_only",
                        AllocationBenchTest::responseStreamingOnce,
                        tmx);
        long streaming =
                measureAlloc(
                        "bidirectional_streaming",
                        AllocationBenchTest::streamingOnce,
                        tmx);
        long async =
                measureAlloc(
                        "async_completable_future",
                        AllocationBenchTest::asyncOnce,
                        tmx);

        System.out.printf(
                "VESPERA_ALLOC summary sync=%d direct=%d resp_streaming=%d bidi_streaming=%d"
                        + " async_caller_side=%d (async completion lands on a Rust Tokio worker"
                        + " thread — not measured here)%n",
                sync, direct, respStreaming, streaming, async);
    }
}
