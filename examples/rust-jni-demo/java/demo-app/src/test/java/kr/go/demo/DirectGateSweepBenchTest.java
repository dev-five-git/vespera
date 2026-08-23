package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Arrays;
import java.util.Map;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;

/**
 * DIRECT-gate sweep — measures {@code DIRECT} vs {@code SYNC} vs
 * {@code BIDIRECTIONAL_STREAMING} dispatch latency across request/response
 * body sizes that straddle the {@link
 * com.devfive.vespera.bridge.SmartDispatchModeResolver} 256&nbsp;KiB gate, to
 * find where DIRECT stops being the cheapest path.
 *
 * <p>{@code POST /echo} returns the request body verbatim, so each size is both
 * the request and the response size.  Gated behind {@code -Dvespera.bench=true}.
 *
 * <p>The crossover is coupled to {@code vespera.direct.maxRetainedBytes} (the
 * pooled direct-buffer retention cap, default 256&nbsp;KiB): a response larger
 * than the cap makes every DIRECT dispatch shrink the buffer, overflow, grow,
 * and <strong>re-run the handler</strong>.  Re-run with
 * {@code -Dvespera.direct.maxRetainedBytes=2097152} (and a matching
 * {@code -Dvespera.direct.maxBufferBytes}) to see DIRECT without that penalty —
 * which is the configuration a raised gate would need.
 */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class DirectGateSweepBenchTest {

    private static final int[] SIZES_KIB = {64, 128, 256, 512, 1024, 1536};
    private static final Map<String, String> HEADERS =
            Map.of("content-type", "application/octet-stream");
    private static long blackhole;

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

    private interface Op {
        int run() throws IOException;
    }

    /**
     * Read the status from a DIRECT response view by copying only the small
     * wire header region (never the body) and decoding it — the controller
     * parses the header straight from the buffer, so charging DIRECT a
     * full-body copy here would be unrepresentative.
     */
    private static int directStatus(ByteBuffer resp) {
        ByteBuffer dup = resp.duplicate().order(ByteOrder.BIG_ENDIAN);
        int headerLen = dup.getInt(0);
        byte[] hdr = new byte[4 + headerLen];
        dup.position(0).get(hdr);
        return VesperaBridge.decodeResponse(hdr).status();
    }

    /** Time-based per-op measurement; returns ns/op over a fixed window. */
    private static long measure(Op op, double warmupSec, double measureSec) throws IOException {
        long warmEnd = System.nanoTime() + (long) (warmupSec * 1e9);
        while (System.nanoTime() < warmEnd) {
            if (op.run() != 200) {
                throw new IllegalStateException("non-200 in warmup");
            }
        }
        long ops = 0;
        long t0 = System.nanoTime();
        long mEnd = t0 + (long) (measureSec * 1e9);
        long now = t0;
        while ((now = System.nanoTime()) < mEnd) {
            blackhole += op.run();
            ops++;
        }
        return (now - t0) / Math.max(ops, 1);
    }

    @Test
    void directGateSweep() throws IOException {
        long retain = Long.getLong("vespera.direct.maxRetainedBytes", 256 * 1024L);
        System.out.printf("VESPERA_BENCH gate_sweep config retain_bytes=%d%n", retain);

        for (int kib : SIZES_KIB) {
            byte[] body = new byte[kib * 1024];
            Arrays.fill(body, (byte) 0xA5);
            byte[] header = VesperaBridge.encodeRequestHeader("POST", "/echo", null, HEADERS);

            Op direct =
                    () ->
                            directStatus(
                                    VesperaBridge.dispatchDirectPooled(
                                            null, "POST", "/echo", null, HEADERS, body, true));
            Op sync =
                    () ->
                            VesperaBridge.decodeResponse(
                                            VesperaBridge.dispatchBytes(
                                                    VesperaBridge.encodeRequest(
                                                            null, "POST", "/echo", null, HEADERS,
                                                            body)))
                                    .status();
            Op bidi =
                    () -> {
                        CountingOutputStream sink = new CountingOutputStream();
                        int[] st = new int[1];
                        VesperaBridge.dispatchFullStreamingWithHeader(
                                header,
                                hb -> st[0] = VesperaBridge.decodeResponse(hb).status(),
                                new ByteArrayInputStream(body),
                                sink);
                        return st[0];
                    };

            // Interleaved 3 rounds (mode round-robin so drift hits all equally),
            // median of the per-round ns/op.
            String[] names = {"direct", "sync", "bidi"};
            Op[] ops = {direct, sync, bidi};
            long[][] roundNs = new long[3][3];
            for (int round = 0; round < 3; round++) {
                for (int m = 0; m < 3; m++) {
                    roundNs[m][round] = measure(ops[m], 0.15, 0.35);
                }
            }
            for (int m = 0; m < 3; m++) {
                long[] sorted = roundNs[m].clone();
                Arrays.sort(sorted);
                System.out.printf(
                        "VESPERA_BENCH gate_sweep size_kib=%d mode=%s ns_per_op=%d retain_bytes=%d%n",
                        kib, names[m], sorted[1], retain);
            }
        }

        if (blackhole == 0) {
            throw new IllegalStateException("blackhole sink optimized away");
        }
    }
}
