import com.devfive.vespera.bridge.VesperaBridge;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

/**
 * JNI dispatch micro-benchmark behind the PR regression gate
 * ({@code .github/workflows/jni-bench-gate.yml}, driven by
 * {@code scripts/jni-bench-interleaved.sh}).
 *
 * <p>Deliberately a single dependency-free source file (no JMH, no Spring,
 * no Gradle at run time): the gate compiles it with {@code javac} against
 * each side's bridge JAR and launches ONE fresh JVM per (side, round), so
 * the merge base and the PR head never share a process — a JVM can load a
 * vespera cdylib only once. The cdylib is the {@code rust-jni-demo} example,
 * found through {@code -Djava.library.path} (the bridge falls back to
 * {@code System.loadLibrary} when the JAR bundles no native library).
 *
 * <p>Method (same idea as {@code SmallRequestLatencyBenchTest}): every mode
 * is warmed up for {@code bench.warmupMillis}, its block size is calibrated
 * so one block lasts about {@code bench.blockMillis}, then the modes are
 * measured ROUND-ROBIN in {@code bench.blocks} blocks so machine drift hits
 * every mode alike; the reported value per mode is the MEDIAN block ns/op
 * (robust to GC-pause / preemption outlier blocks). Each op runs the full
 * caller path: encode the wire request, cross JNI, decode the response
 * status — Java codec regressions are therefore covered too.
 *
 * <p>Output: one {@code RESULT <mode> <ns_per_op>} line per mode on stdout.
 * Exit code 1 when any mode fails its response check.
 */
public final class JniDispatchBench {

    private static final Map<String, String> GET_HEADERS = Map.of("accept", "text/plain");
    private static final int POST_BYTES = 64 * 1024;
    private static final byte[] POST_BODY = new byte[POST_BYTES];
    private static final Map<String, String> POST_HEADERS = Map.of(
            "content-type", "application/octet-stream",
            "content-length", Integer.toString(POST_BYTES));

    static {
        for (int i = 0; i < POST_BODY.length; i++) {
            POST_BODY[i] = (byte) (i * 31 + 7);
        }
    }

    private static final ByteBuffer DIRECT_IN = ByteBuffer.allocateDirect(4 * 1024);
    private static final ByteBuffer DIRECT_OUT = ByteBuffer.allocateDirect(64 * 1024);

    /** Byte-counting sink: the response body is never retained. */
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

    @FunctionalInterface
    private interface Op {
        /** Runs one request; returns the HTTP status (the value is also a JIT sink). */
        int run() throws Exception;
    }

    // ── modes ────────────────────────────────────────────────────────────

    /** SYNC: {@code dispatchBytes}, small GET. */
    private static int syncGet() {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, GET_HEADERS, null);
        return VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire)).status();
    }

    /** DIRECT: raw {@code dispatchDirect} with caller-owned direct buffers, small GET. */
    private static int directGet() {
        int inLen = VesperaBridge.encodeRequestInto(
                null, "GET", "/health", null, GET_HEADERS, null, DIRECT_IN);
        int n = VesperaBridge.dispatchDirect(DIRECT_IN, inLen, DIRECT_OUT);
        if (n < 0) {
            throw new IllegalStateException("direct response overflow: " + n);
        }
        byte[] wire = new byte[n];
        DIRECT_OUT.get(0, wire, 0, n);
        return VesperaBridge.decodeResponse(wire).status();
    }

    /** Response streaming: {@code dispatchStreaming}, small GET. */
    private static int streamingGet() throws IOException {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, GET_HEADERS, null);
        CountingOutputStream sink = new CountingOutputStream();
        byte[] header = VesperaBridge.dispatchStreaming(wire, sink);
        return VesperaBridge.decodeResponse(header).status();
    }

    /** Bidirectional streaming: {@code dispatchFullStreaming}, 64 KiB POST echoed back. */
    private static int fullStreamingPost64k() throws IOException {
        byte[] header = VesperaBridge.encodeRequestHeader("POST", "/echo", null, POST_HEADERS);
        CountingOutputStream sink = new CountingOutputStream();
        byte[] respHeader = VesperaBridge.dispatchFullStreaming(
                header, new ByteArrayInputStream(POST_BODY), sink);
        int status = VesperaBridge.decodeResponse(respHeader).status();
        if (sink.count != POST_BYTES) {
            throw new IllegalStateException("echo returned " + sink.count + " bytes");
        }
        return status;
    }

    /** ASYNC: {@code dispatchAsync} + blocking wait, small GET (includes the cross-thread wake-up). */
    private static int asyncGet() throws Exception {
        byte[] wire = VesperaBridge.encodeRequest(null, "GET", "/health", null, GET_HEADERS, null);
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        VesperaBridge.dispatchAsync(future, wire);
        return VesperaBridge.decodeResponse(future.get(30, TimeUnit.SECONDS)).status();
    }

    // ── harness ──────────────────────────────────────────────────────────

    private static long longProp(String key, long def) {
        String v = System.getProperty(key);
        return v == null || v.isBlank() ? def : Long.parseLong(v.trim());
    }

    public static void main(String[] args) throws Exception {
        long warmupMillis = longProp("bench.warmupMillis", 1500);
        long blockMillis = longProp("bench.blockMillis", 20);
        int blocks = (int) longProp("bench.blocks", 40);

        VesperaBridge.init(System.getProperty("bench.library", "rust_jni_demo"));

        String[] names = {
            "sync_get", "direct_get", "streaming_get", "full_streaming_post_64k", "async_get",
        };
        Op[] ops = {
            JniDispatchBench::syncGet,
            JniDispatchBench::directGet,
            JniDispatchBench::streamingGet,
            JniDispatchBench::fullStreamingPost64k,
            JniDispatchBench::asyncGet,
        };

        // Warm up each mode (JIT + native caches) and calibrate its block size.
        int[] blockIters = new int[ops.length];
        for (int m = 0; m < ops.length; m++) {
            long deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(warmupMillis);
            long iters = 0;
            long t0 = System.nanoTime();
            while (System.nanoTime() < deadline) {
                int status = ops[m].run();
                if (status != 200) {
                    System.err.println(names[m] + ": unexpected status " + status);
                    System.exit(1);
                }
                iters++;
            }
            double nsPerOp = (double) (System.nanoTime() - t0) / Math.max(1, iters);
            blockIters[m] = (int) Math.max(16, TimeUnit.MILLISECONDS.toNanos(blockMillis) / nsPerOp);
        }

        long[][] blockNs = new long[ops.length][blocks];
        long sink = 0;
        for (int b = 0; b < blocks; b++) {
            for (int m = 0; m < ops.length; m++) {
                int n = blockIters[m];
                long t0 = System.nanoTime();
                for (int i = 0; i < n; i++) {
                    sink += ops[m].run();
                }
                blockNs[m][b] = (System.nanoTime() - t0) / n;
            }
        }
        long expected = 0;
        for (int n : blockIters) {
            expected += 200L * n * blocks;
        }
        if (sink != expected) {
            System.err.println("non-200 responses during measurement (status sum " + sink + ")");
            System.exit(1);
        }

        for (int m = 0; m < ops.length; m++) {
            long[] sorted = blockNs[m].clone();
            Arrays.sort(sorted);
            long median = sorted[blocks / 2];
            System.out.printf("RESULT %s %d%n", names[m], median);
            System.err.printf("%-24s %8d ns/op  (median of %d blocks x %d ops; min %d max %d)%n",
                    names[m], median, blocks, blockIters[m], sorted[0], sorted[blocks - 1]);
        }
        VesperaBridge.shutdown(java.time.Duration.ofSeconds(5));
    }

    private JniDispatchBench() {}
}
