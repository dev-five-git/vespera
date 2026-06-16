package com.devfive.vespera.bridge;

import com.sun.management.ThreadMXBean;
import java.lang.management.ManagementFactory;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.function.BiConsumer;
import java.util.function.IntConsumer;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;
import org.springframework.mock.web.MockHttpServletRequest;

/**
 * Allocation microbenchmark for the per-request controller / wire-reader hot
 * paths optimized by P3 (WireHeaderReader.apply canonical header-key reuse) and
 * P1 (readBody bodyless skip). Uses the same
 * {@code ThreadMXBean.getThreadAllocatedBytes} idiom as the demo-app
 * {@code AllocationBenchTest} — allocation-per-op is deterministic (unlike
 * timing), so it is the noise-free signal for these allocation-reduction wins.
 *
 * <p>Opt-in (like the demo-app benches): run with
 * {@code ./gradlew test --tests "*PerfAllocBench*" -Dvespera.bench=true}.
 * Compare the printed {@code VESPERA_ALLOC} lines before vs after.
 */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class PerfAllocBench {

    private static final int WARMUP = 5_000;
    private static final int MEASURE = 100_000;

    /** Realistic response wire header: 5 canonical keys + 1 non-canonical. */
    private static final byte[] RESP_WIRE = buildRespWire();

    private static byte[] buildRespWire() {
        String json =
                "{\"v\":1,\"status\":200,\"headers\":{"
                        + "\"content-type\":\"application/json\","
                        + "\"content-length\":\"256\","
                        + "\"cache-control\":\"no-store\","
                        + "\"etag\":\"\\\"abc123\\\"\","
                        + "\"vary\":\"accept-encoding\","
                        + "\"x-request-id\":\"01HV2N3M4P5Q6R7S8T9V0W1X2Y\""
                        + "},\"metadata\":{\"version\":\"0.1.0\"}}";
        byte[] hb = json.getBytes(StandardCharsets.UTF_8);
        ByteBuffer buf = ByteBuffer.allocate(4 + hb.length);
        buf.putInt(hb.length);
        buf.put(hb);
        return buf.array();
    }

    private static ThreadMXBean threadMx() {
        java.lang.management.ThreadMXBean base = ManagementFactory.getThreadMXBean();
        Assumptions.assumeTrue(
                base instanceof ThreadMXBean,
                "non-HotSpot JVM — no com.sun.management.ThreadMXBean");
        ThreadMXBean tmx = (ThreadMXBean) base;
        Assumptions.assumeTrue(
                tmx.isThreadAllocatedMemorySupported(), "thread allocation not supported");
        if (!tmx.isThreadAllocatedMemoryEnabled()) {
            tmx.setThreadAllocatedMemoryEnabled(true);
        }
        return tmx;
    }

    @Test
    void p3_apply_bytesPerOp() {
        ThreadMXBean tmx = threadMx();
        long tid = Thread.currentThread().getId();
        int hb = RESP_WIRE.length - 4;
        ByteBuffer buf = ByteBuffer.wrap(RESP_WIRE);

        long[] keyLenSink = {0};
        int[] statusSink = {0};
        IntConsumer onStatus = s -> statusSink[0] = s;
        BiConsumer<String, String> onHeader = (k, v) -> keyLenSink[0] += k.length() + v.length();

        for (int i = 0; i < WARMUP; i++) {
            WireHeaderReader.apply(buf, 4, hb, onStatus, onHeader);
        }
        long before = tmx.getThreadAllocatedBytes(tid);
        for (int i = 0; i < MEASURE; i++) {
            WireHeaderReader.apply(buf, 4, hb, onStatus, onHeader);
        }
        long after = tmx.getThreadAllocatedBytes(tid);
        long bytesPerOp = (after - before) / MEASURE;
        System.out.printf(
                "VESPERA_ALLOC p3_apply bytes_per_op=%d (6 headers: 5 canonical + 1 other;"
                        + " status=%d keyLenSink=%d)%n",
                bytesPerOp, statusSink[0], keyLenSink[0]);
    }

    @Test
    void p1_readBody_bodylessGet_bytesPerOp() throws Exception {
        ThreadMXBean tmx = threadMx();
        long tid = Thread.currentThread().getId();
        long sink = 0;

        // A fresh MockHttpServletRequest each iteration; its allocation is
        // identical before vs after, so it cancels in the before/after delta —
        // the delta isolates readBody's own allocation (getInputStream wrapper +
        // readAllBytes buffers, which the bodyless fast path skips).
        for (int i = 0; i < WARMUP; i++) {
            sink += VesperaProxyController.readBody(new MockHttpServletRequest("GET", "/health")).length;
        }
        long before = tmx.getThreadAllocatedBytes(tid);
        for (int i = 0; i < MEASURE; i++) {
            sink += VesperaProxyController.readBody(new MockHttpServletRequest("GET", "/health")).length;
        }
        long after = tmx.getThreadAllocatedBytes(tid);
        long bytesPerOp = (after - before) / MEASURE;
        System.out.printf(
                "VESPERA_ALLOC p1_readBody_bodyless bytes_per_op=%d"
                        + " (incl. constant MockHttpServletRequest alloc; sink=%d)%n",
                bytesPerOp, sink);
    }
}
