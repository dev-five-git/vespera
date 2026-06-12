package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.devfive.vespera.bridge.VesperaBridge;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;

/**
 * E2E small-request latency benchmark through the REAL JNI boundary —
 * quantifies what {@code vespera.bridge.dispatch-mode=smart} buys for
 * the requests it targets (small bounded idempotent), by comparing the
 * three dispatch modes on the same tiny {@code GET /health} round-trip:
 *
 * <ul>
 *   <li>{@code SYNC} — {@code encodeRequest} → {@code dispatchBytes}
 *       → {@code decodeResponse} (two JNI array copies)</li>
 *   <li>{@code DIRECT} — {@code dispatchDirectPooled} fast path
 *       (pooled direct buffers, no Java heap arrays)</li>
 *   <li>{@code BIDIRECTIONAL_STREAMING} — the autoconfigured default
 *       ({@code dispatchFullStreamingWithHeader})</li>
 * </ul>
 *
 * <p>Gated behind {@code -Dvespera.bench=true} so normal test runs and
 * CI skip it:
 *
 * <pre>
 *   ./gradlew :demo-app:test --tests "*SmallRequestLatencyBenchTest*" \
 *       -Dvespera.bench=true
 * </pre>
 */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class SmallRequestLatencyBenchTest {

    private static final int WARMUP = 20_000;
    private static final int ITERS = 100_000;
    private static final Map<String, String> HEADERS = Map.of("accept", "application/json");

    @BeforeAll
    static void setUp() {
        VesperaBridge.init("rust_jni_demo");
    }

    /** OutputStream that counts bytes without storing them. */
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

    private static int asyncOnce() {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, HEADERS, null);
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        VesperaBridge.dispatchAsync(future, wire);
        try {
            byte[] resp = future.get(30, TimeUnit.SECONDS);
            return VesperaBridge.decodeResponse(resp).status();
        } catch (Exception e) {
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

    private static long measure(String name, Op op) throws IOException {
        for (int i = 0; i < WARMUP; i++) {
            assertEquals(200, op.run(), name + " warmup status");
        }
        long blackhole = 0;
        long t0 = System.nanoTime();
        for (int i = 0; i < ITERS; i++) {
            blackhole += op.run();
        }
        long nsPerOp = (System.nanoTime() - t0) / ITERS;
        System.out.printf(
                "VESPERA_BENCH small_request mode=%s ns_per_op=%d (blackhole %d)%n",
                name, nsPerOp, blackhole);
        return nsPerOp;
    }

    @Test
    void smallRequestLatencyByMode() throws IOException {
        long sync = measure("sync_dispatch_bytes", SmallRequestLatencyBenchTest::syncOnce);
        long direct = measure("direct_pooled", SmallRequestLatencyBenchTest::directOnce);
        long respStreaming =
                measure(
                        "response_streaming_only",
                        SmallRequestLatencyBenchTest::responseStreamingOnce);
        long streaming =
                measure("bidirectional_streaming", SmallRequestLatencyBenchTest::streamingOnce);
        long async =
                measure(
                        "async_completable_future",
                        SmallRequestLatencyBenchTest::asyncOnce);
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
