package com.devfive.vespera.bridge;

import com.sun.management.ThreadMXBean;
import java.lang.management.ManagementFactory;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Map;
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

    @Test
    void proxyHeaderEncode_bytesPerOp() {
        ThreadMXBean tmx = threadMx();
        long tid = Thread.currentThread().getId();
        MockHttpServletRequest req = realisticHeaderRequest();
        long sink = 0;

        for (int i = 0; i < WARMUP; i++) {
            Map<String, String> headers = VesperaProxyController.collectHeaders(req);
            sink += VesperaBridge.encodeRequest(null, "GET", "/x", null, headers, null).length;
        }
        long oldBefore = tmx.getThreadAllocatedBytes(tid);
        for (int i = 0; i < MEASURE; i++) {
            Map<String, String> headers = VesperaProxyController.collectHeaders(req);
            sink += VesperaBridge.encodeRequest(null, "GET", "/x", null, headers, null).length;
        }
        long oldAfter = tmx.getThreadAllocatedBytes(tid);
        long oldBytesPerOp = (oldAfter - oldBefore) / MEASURE;

        for (int i = 0; i < WARMUP; i++) {
            sink += VesperaBridge.encodeRequest(null, "GET", "/x", null,
                    (VesperaBridge.HeaderSource) (s -> VesperaProxyController.forEachRequestHeader(req, s)),
                    null).length;
        }
        long newBefore = tmx.getThreadAllocatedBytes(tid);
        for (int i = 0; i < MEASURE; i++) {
            sink += VesperaBridge.encodeRequest(null, "GET", "/x", null,
                    (VesperaBridge.HeaderSource) (s -> VesperaProxyController.forEachRequestHeader(req, s)),
                    null).length;
        }
        long newAfter = tmx.getThreadAllocatedBytes(tid);
        long newBytesPerOp = (newAfter - newBefore) / MEASURE;

        System.out.printf(
                "VESPERA_ALLOC proxy_header_encode_old bytes_per_op=%d (sink=%d)%n",
                oldBytesPerOp, sink);
        System.out.printf(
                "VESPERA_ALLOC proxy_header_encode_new bytes_per_op=%d (sink=%d)%n",
                newBytesPerOp, sink);
    }

    /** Reusable per-thread scratch for the JVM-02 "after" path. */
    private static final ThreadLocal<byte[]> DIRECT_SCRATCH = ThreadLocal.withInitial(() -> new byte[0]);

    private static final int DIRECT_SCRATCH_CAP = 256 * 1024;

    /**
     * JVM-02 before/after allocation A/B for the DIRECT response body
     * write. {@code before} bridges the direct {@link ByteBuffer} to the
     * servlet {@link java.io.OutputStream} via a fresh
     * {@link java.nio.channels.Channels#newChannel} per call (which
     * allocates a channel object + an internal heap transfer buffer every
     * time); {@code after} copies through a reusable per-thread
     * {@code byte[]} scratch. Allocation-per-op is the deterministic,
     * noise-free signal for this allocation-removal win.
     */
    @Test
    void directResponseWrite_bytesPerOp() throws Exception {
        ThreadMXBean tmx = threadMx();
        long tid = Thread.currentThread().getId();

        int payload = 8 * 1024;
        ByteBuffer src = ByteBuffer.allocateDirect(payload);
        for (int i = 0; i < payload; i++) {
            src.put((byte) (i & 0x7f));
        }
        // Discarding sink — mirrors writing to a committed servlet
        // OutputStream without measuring the servlet container itself.
        java.io.OutputStream sink =
                new java.io.OutputStream() {
                    @Override
                    public void write(int b) {}

                    @Override
                    public void write(byte[] b, int off, int len) {}

                    @Override
                    public void write(byte[] b) {}
                };

        for (int i = 0; i < WARMUP; i++) {
            directWriteBefore(src, sink);
        }
        long ob = tmx.getThreadAllocatedBytes(tid);
        for (int i = 0; i < MEASURE; i++) {
            directWriteBefore(src, sink);
        }
        long oa = tmx.getThreadAllocatedBytes(tid);
        long beforeBpo = (oa - ob) / MEASURE;

        for (int i = 0; i < WARMUP; i++) {
            directWriteAfter(src, sink);
        }
        long nb = tmx.getThreadAllocatedBytes(tid);
        for (int i = 0; i < MEASURE; i++) {
            directWriteAfter(src, sink);
        }
        long na = tmx.getThreadAllocatedBytes(tid);
        long afterBpo = (na - nb) / MEASURE;

        System.out.printf(
                "VESPERA_ALLOC direct_resp_write_before bytes_per_op=%d (8 KiB direct body)%n",
                beforeBpo);
        System.out.printf(
                "VESPERA_ALLOC direct_resp_write_after  bytes_per_op=%d (8 KiB direct body)%n",
                afterBpo);
    }

    private static void directWriteBefore(ByteBuffer src, java.io.OutputStream out)
            throws Exception {
        src.clear();
        java.nio.channels.WritableByteChannel ch = java.nio.channels.Channels.newChannel(out);
        while (src.hasRemaining()) {
            ch.write(src);
        }
    }

    private static void directWriteAfter(ByteBuffer src, java.io.OutputStream out)
            throws Exception {
        src.clear();
        int needed = Math.min(src.remaining(), DIRECT_SCRATCH_CAP);
        byte[] scratch = DIRECT_SCRATCH.get();
        if (scratch.length < needed) {
            scratch = new byte[needed];
            DIRECT_SCRATCH.set(scratch);
        }
        while (src.hasRemaining()) {
            int chunk = Math.min(scratch.length, src.remaining());
            src.get(scratch, 0, chunk);
            out.write(scratch, 0, chunk);
        }
    }

    /**
     * JVM-04 before/after for the per-thread header-buffer retention.
     * The buffer is a private heap {@code byte[]} only exercised through
     * the native dispatch path, so this models the two retention
     * policies over a representative request sequence and measures the
     * RETAINED capacity (the memory-footprint signal, not allocation
     * rate). Production constants verified: {@code HEADER_INITIAL=256},
     * {@code HEADER_RETAIN=32 KiB}. {@code before} grows and never
     * shrinks; {@code after} drops back to 256 once it exceeds 32 KiB.
     */
    @Test
    void headerBufRetention_retainedBytes() {
        final int initial = 256;
        final int retainCap = 32 * 1024;
        final int hugeHeader = 64 * 1024; // one fat cookie/header burst
        final int normalHeader = 256;
        final int normalRequests = 1000;

        // BEFORE: monotonic grow, never shrink — one fat request pins the
        // backing array for the rest of that servlet thread's life.
        int beforeCap = initial;
        beforeCap = Math.max(beforeCap, hugeHeader);
        for (int i = 0; i < normalRequests; i++) {
            beforeCap = Math.max(beforeCap, normalHeader);
        }

        // AFTER: reset to initial whenever capacity exceeds the retain cap.
        int afterCap = initial;
        afterCap = Math.max(afterCap, hugeHeader);
        if (afterCap > retainCap) {
            afterCap = initial;
        }
        for (int i = 0; i < normalRequests; i++) {
            afterCap = Math.max(afterCap, normalHeader);
            if (afterCap > retainCap) {
                afterCap = initial;
            }
        }

        System.out.printf(
                "VESPERA_ALLOC header_buf_retained_before bytes=%d (pinned after one 64 KiB header)%n",
                beforeCap);
        System.out.printf(
                "VESPERA_ALLOC header_buf_retained_after  bytes=%d (reset below 32 KiB cap)%n",
                afterCap);
    }

    /**
     * JVM-05 before/after for the direct-buffer pool retention. The
     * pooled buffers are off-heap direct {@link ByteBuffer}s only
     * exercised through the native dispatch path, so this models the two
     * policies over a repeated-large-response sequence and counts the
     * multi-MiB direct (re)allocations — each {@code before} realloc also
     * forces a Rust handler re-run on the overflow retry. Production
     * constants verified: {@code DIRECT_INITIAL=64 KiB},
     * {@code DIRECT_SHRINK_IDLE_DISPATCHES=8}. {@code before} shrinks to
     * initial at the start of every dispatch; {@code after} keeps the
     * grown buffer while it stays in use.
     */
    @Test
    void directPoolRetention_reallocations() {
        // Production defaults: DIRECT_INITIAL 64 KiB, DIRECT_RETAIN 2 MiB,
        // DIRECT_MAX 4 MiB.  The modelled response must exceed the retain
        // cap (so the policies diverge) yet fit within the max cap (so it
        // stays on the pooled direct path instead of the heap fallback) —
        // 3 MiB satisfies both.
        final int initial = 64 * 1024;
        final int retainCap = 2 * 1024 * 1024;
        final int reqSize = 3 * 1024 * 1024; // repeated 3 MiB idempotent response
        final int dispatches = 50;

        // BEFORE: eager shrink at the start of each dispatch → every
        // dispatch re-grows (reallocates) the big buffer AND re-runs the
        // Rust handler on the overflow retry.
        int beforeReallocs = 0;
        int beforeRehandlers = 0;
        int beforeCap = initial;
        for (int i = 0; i < dispatches; i++) {
            if (beforeCap > retainCap) {
                beforeCap = initial; // eager shrink
            }
            if (beforeCap < reqSize) {
                beforeCap = reqSize;
                beforeReallocs++;
                beforeRehandlers++; // overflow → retry re-runs the handler
            }
        }

        // AFTER: adaptive — keep the grown buffer while repeatedly used;
        // shrink only after 8 consecutive under-retain dispatches.
        int afterReallocs = 0;
        int afterRehandlers = 0;
        int afterCap = initial;
        int idle = 0;
        for (int i = 0; i < dispatches; i++) {
            if (idle >= 8 && afterCap > retainCap) {
                afterCap = initial;
                idle = 0;
            }
            if (afterCap < reqSize) {
                afterCap = reqSize;
                afterReallocs++;
                afterRehandlers++;
            }
            idle = (reqSize <= retainCap) ? idle + 1 : 0;
        }

        System.out.printf(
                "VESPERA_ALLOC direct_pool_reallocs_before count=%d handler_reruns=%d (%d dispatches, %d MiB each)%n",
                beforeReallocs, beforeRehandlers, dispatches, reqSize / (1024 * 1024));
        System.out.printf(
                "VESPERA_ALLOC direct_pool_reallocs_after  count=%d handler_reruns=%d%n",
                afterReallocs, afterRehandlers);
    }

    private static MockHttpServletRequest realisticHeaderRequest() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("Host", "api.example.test");
        req.addHeader("User-Agent", "Mozilla/5.0 vespera-bench");
        req.addHeader("Accept", "application/json");
        req.addHeader("Accept-Encoding", "gzip, br");
        req.addHeader("Accept-Language", "en-US,en;q=0.9");
        req.addHeader("Cache-Control", "no-cache");
        req.addHeader("Cookie", "sid=abc");
        req.addHeader("Cookie", "theme=dark");
        req.addHeader("X-Request-Id", "01HV2N3M4P5Q6R7S8T9V0W1X2Y");
        req.addHeader("X-Forwarded-For", "203.0.113.10");
        req.addHeader("X-Forwarded-Proto", "https");
        req.addHeader("X-Vespera-App", "admin");
        return req;
    }
}
