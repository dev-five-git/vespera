package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.condition.EnabledIfSystemProperty;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.util.Map;
import java.util.Random;

import static org.junit.jupiter.api.Assertions.assertEquals;

/**
 * E2E streaming throughput benchmark through the REAL JNI boundary —
 * measures {@code dispatchFullStreamingWithHeader} (the autoconfigured
 * default dispatch mode) round-tripping a large body through the Rust
 * {@code /echo} route.
 *
 * <p>The streaming chunk size is <strong>process-fixed</strong> after
 * the first dispatch, so each chunk size needs its own JVM. Run via:
 *
 * <pre>
 *   ./gradlew :demo-app:test --tests "*StreamingThroughputBenchTest*" \
 *       -Dvespera.bench=true -Dvespera.streaming.chunkBytes=16384
 * </pre>
 *
 * <p>Gated behind {@code -Dvespera.bench=true} so normal test runs and
 * CI skip it.
 */
@EnabledIfSystemProperty(named = "vespera.bench", matches = "true")
class StreamingThroughputBenchTest {

    private static final int PAYLOAD_BYTES = 64 * 1024 * 1024; // 64 MiB
    private static final int WARMUP_ITERATIONS = 3;
    private static final int MEASURE_ITERATIONS = 10;

    private static byte[] payload;

    @BeforeAll
    static void setUp() {
        VesperaBridge.init("rust_jni_demo");
        payload = new byte[PAYLOAD_BYTES];
        new Random(42).nextBytes(payload);
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

    private static long roundTripOnce() throws IOException {
        byte[] wireHeader = VesperaBridge.encodeRequestHeader(
                "POST", "/echo/stream", null,
                Map.of("content-type", "application/octet-stream"));
        CountingOutputStream sink = new CountingOutputStream();
        int[] status = new int[1];
        VesperaBridge.dispatchFullStreamingWithHeader(
                wireHeader,
                headerBytes -> status[0] = VesperaBridge.decodeResponse(headerBytes).status(),
                new ByteArrayInputStream(payload),
                sink);
        assertEquals(200, status[0], "echo status");
        assertEquals(PAYLOAD_BYTES, sink.count, "echoed byte count");
        return sink.count;
    }

    @Test
    void bidirectionalStreamingThroughput() throws IOException {
        String chunkProp = System.getProperty("vespera.streaming.chunkBytes", "default(65536)");

        for (int i = 0; i < WARMUP_ITERATIONS; i++) {
            roundTripOnce();
        }

        double[] mibPerSec = new double[MEASURE_ITERATIONS];
        for (int i = 0; i < MEASURE_ITERATIONS; i++) {
            long t0 = System.nanoTime();
            roundTripOnce();
            long elapsedNs = System.nanoTime() - t0;
            // Bidirectional: payload travels Java→Rust AND Rust→Java.
            mibPerSec[i] = (PAYLOAD_BYTES / (1024.0 * 1024.0)) / (elapsedNs / 1_000_000_000.0);
        }

        double mean = 0;
        for (double v : mibPerSec) mean += v;
        mean /= MEASURE_ITERATIONS;
        double var = 0;
        for (double v : mibPerSec) var += (v - mean) * (v - mean);
        double stddev = Math.sqrt(var / MEASURE_ITERATIONS);

        System.out.printf(
                "VESPERA_BENCH chunkBytes=%s payload=%d MiB iterations=%d"
                        + " throughput=%.1f MiB/s stddev=%.1f%n",
                chunkProp, PAYLOAD_BYTES / (1024 * 1024), MEASURE_ITERATIONS, mean, stddev);
    }
}
