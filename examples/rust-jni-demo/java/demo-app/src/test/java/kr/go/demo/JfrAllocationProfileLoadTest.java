package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.Map;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;

/** Sustained single-threaded JNI load for allocation profiling under JFR. */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class JfrAllocationProfileLoadTest {

    private static final int WARMUP_SECONDS = 1;
    private static final int LOAD_SECONDS = 10;
    private static final Map<String, String> HEADERS = Map.of("accept", "application/json");

    @BeforeAll
    static void setUp() {
        VesperaBridge.init("rust_jni_demo");
    }

    // Mode implementations: intentionally equivalent to SmallRequestLatencyBenchTest
    // so JFR samples map to the same helper paths as the latency/allocation benches.
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

    private interface Op {
        int run() throws IOException;
    }

    private static void warmup(String mode, Op op) throws IOException {
        long until = System.nanoTime() + TimeUnit.SECONDS.toNanos(WARMUP_SECONDS);
        while (System.nanoTime() < until) {
            if (op.run() != 200) {
                throw new IllegalStateException(mode + " warmup non-200");
            }
        }
    }

    private static void load(String mode, Op op) throws IOException {
        warmup(mode, op);

        long ops = 0;
        long started = System.nanoTime();
        long until = started + TimeUnit.SECONDS.toNanos(LOAD_SECONDS);
        while (System.nanoTime() < until) {
            if (op.run() != 200) {
                throw new IllegalStateException(mode + " load non-200");
            }
            ops++;
        }
        double seconds = (System.nanoTime() - started) / 1_000_000_000.0;
        System.out.printf(
                "VESPERA_JFR_LOAD %s ops_per_sec=%.0f total_ops=%d seconds=%.2f%n",
                mode, ops / seconds, ops, seconds);
    }

    @Test
    void sustainedSyncAndDirectLoad() throws IOException {
        System.out.printf(
                "VESPERA_JFR_LOAD warmup_seconds=%d load_seconds_per_mode=%d%n",
                WARMUP_SECONDS, LOAD_SECONDS);
        load("sync_dispatch_bytes", JfrAllocationProfileLoadTest::syncOnce);
        load("direct_pooled", JfrAllocationProfileLoadTest::directOnce);
    }
}
