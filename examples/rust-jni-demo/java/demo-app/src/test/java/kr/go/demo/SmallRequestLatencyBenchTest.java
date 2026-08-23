package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.devfive.vespera.bridge.VesperaBridge;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;

/** E2E JNI latency benchmark gated behind {@code -Dvespera.bench=true}. */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class SmallRequestLatencyBenchTest {

    private static final int WARMUP = 20_000;
    private static final int ITERS = 100_000;
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

    /**
     * Async-then-synchronously-block — the WORST case for {@code CompletableFuture}.
     * The ~15us/op this measures is dominated (~5-8us) by the caller thread parking
     * on {@code future.get()} and being woken cross-thread after the Rust Tokio
     * worker completes the future: OS-scheduler park/unpark latency, NOT Rust
     * dispatch cost (~2us — see the sync/direct/streaming modes). Real async
     * consumers chain continuations ({@code thenApply}/{@code thenCompose}) and
     * never pay this park/wake. Treat this mode's absolute number as a cross-thread
     * handoff-latency probe, not a dispatch-cost regression signal — watch the
     * ratios and the other modes for dispatch regressions.
     */
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

    /** Response-streaming only — no request pull thread (empty body inline). */
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
     * Interleaved, median-of-blocks latency measurement.
     *
     * <p>Modes are measured round-robin in small blocks instead of one long
     * run each, so machine drift (CPU boost / thermal / background load) hits
     * every mode equally within a round — the cross-mode RATIOS become
     * noise-robust even when absolute ns/op drift {@code ±10%} run-to-run. Per
     * mode the MEDIAN of the per-block ns/op is reported, which is robust to
     * GC-pause outlier blocks. This is what makes the numbers trustworthy
     * enough to watch for regressions in CI (see {@code jni-bench.yml}).
     */
    private static long[] measureInterleaved(String[] names, Op[] ops) throws IOException {
        final int rounds = 100;
        final int block = ITERS / rounds; // 1000 iters/block, 100 blocks/mode

        // Warm up every mode fully (JIT, code cache) before any measurement.
        for (int m = 0; m < ops.length; m++) {
            for (int i = 0; i < WARMUP; i++) {
                assertEquals(200, ops[m].run(), names[m] + " warmup status");
            }
        }

        long[][] blockNs = new long[ops.length][rounds];
        long blackhole = 0;
        for (int r = 0; r < rounds; r++) {
            for (int m = 0; m < ops.length; m++) {
                long t0 = System.nanoTime();
                for (int i = 0; i < block; i++) {
                    blackhole += ops[m].run();
                }
                blockNs[m][r] = (System.nanoTime() - t0) / block;
            }
        }
        if (blackhole == 0) {
            throw new IllegalStateException("blackhole sink optimized away");
        }

        long[] medianNs = new long[ops.length];
        for (int m = 0; m < ops.length; m++) {
            long[] sorted = blockNs[m].clone();
            java.util.Arrays.sort(sorted);
            medianNs[m] = sorted[sorted.length / 2];
            System.out.printf(
                    "VESPERA_BENCH small_request mode=%s ns_per_op=%d"
                            + " (interleaved median rounds=%d block=%d)%n",
                    names[m], medianNs[m], rounds, block);
        }
        return medianNs;
    }

    @Test
    void smallRequestLatencyByMode() throws IOException {
        String[] names = {
            "sync_dispatch_bytes",
            "direct_pooled",
            "response_streaming_only",
            "bidirectional_streaming",
            "async_completable_future",
        };
        Op[] ops = {
            SmallRequestLatencyBenchTest::syncOnce,
            SmallRequestLatencyBenchTest::directOnce,
            SmallRequestLatencyBenchTest::responseStreamingOnce,
            SmallRequestLatencyBenchTest::streamingOnce,
            SmallRequestLatencyBenchTest::asyncOnce,
        };
        long[] ns = measureInterleaved(names, ops);
        long sync = ns[0];
        long direct = ns[1];
        long respStreaming = ns[2];
        long streaming = ns[3];
        long async = ns[4];

        // Cross-mode ratios are the NOISE-ROBUST regression signal: every mode
        // was measured under the same interleaved machine state, so these
        // ratios stay stable run-to-run even when absolute ns/op drift ±10%.
        System.out.printf(
                "VESPERA_BENCH summary direct_vs_streaming=%.2fx direct_vs_sync=%.2fx"
                        + " resp_only_vs_bidi=%.2fx async_vs_sync=%.2fx async_vs_direct=%.2fx%n",
                (double) streaming / direct,
                (double) sync / direct,
                (double) streaming / respStreaming,
                (double) async / sync,
                (double) async / direct);
    }
}
