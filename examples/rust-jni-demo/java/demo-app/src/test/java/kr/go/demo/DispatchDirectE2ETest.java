package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.MethodOrderer;
import org.junit.jupiter.api.Order;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestMethodOrder;

import java.nio.ByteBuffer;
import java.security.MessageDigest;
import java.util.Map;
import java.util.Random;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * End-to-end tests for the DirectByteBuffer dispatch path — loads the
 * real {@code rust_jni_demo} cdylib (bundled into test resources by the
 * vespera Gradle plugin) and proves {@code dispatchDirect*} produces
 * byte-identical wire responses to {@code dispatchBytes}.
 *
 * <p>{@code /echo} round-trips the request body verbatim, so request
 * size == response body size — convenient for exercising the pooled
 * out-buffer growth (64 KiB initial) and the overflow protocol.
 */
@TestMethodOrder(MethodOrderer.OrderAnnotation.class)
class DispatchDirectE2ETest {

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
    }

    private static byte[] echoWire(byte[] body) {
        return VesperaBridge.encodeRequest(
                "POST", "/echo", null,
                Map.of("content-type", "application/octet-stream"),
                body);
    }

    private static byte[] randomBody(int size, long seed) {
        byte[] body = new byte[size];
        new Random(seed).nextBytes(body);
        return body;
    }

    private static byte[] toArray(ByteBuffer view) {
        byte[] out = new byte[view.remaining()];
        view.get(out);
        return out;
    }

    private static byte[] sha256(byte[] data) throws Exception {
        return MessageDigest.getInstance("SHA-256").digest(data);
    }

    /**
     * The DIRECT response must be semantically identical to the
     * dispatchBytes response: same status, same headers, SHA256-equal
     * body.  (Raw wire bytes are NOT compared — the wire header JSON
     * serialises a Rust HashMap whose key order is intentionally
     * unspecified per response.)
     */
    private static void assertDirectMatchesBytes(int bodySize, long seed) throws Exception {
        byte[] wire = echoWire(randomBody(bodySize, seed));

        VesperaBridge.DecodedResponse viaBytes =
                VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire));
        VesperaBridge.DecodedResponse viaDirect =
                VesperaBridge.decodeResponse(
                        toArray(VesperaBridge.dispatchDirectPooled(wire, true)));

        assertEquals(200, viaDirect.status());
        assertEquals(viaBytes.status(), viaDirect.status(), "status");
        assertEquals(viaBytes.headers(), viaDirect.headers(), "headers");
        assertEquals(bodySize, viaDirect.body().length, "body length");
        assertArrayEquals(sha256(viaBytes.body()), sha256(viaDirect.body()),
                "body must be byte-identical for size " + bodySize);
    }

    @Test
    @Order(1)
    void tinyBodyFitsInitialBuffer() throws Exception {
        assertDirectMatchesBytes(1024, 1);
    }

    @Test
    @Order(2)
    void mediumBodyTriggersOutBufferGrowth() throws Exception {
        // 100 KiB response > 64 KiB initial out buffer → overflow →
        // grow → re-dispatch (retryOnOverflow=true; /echo is safe).
        assertDirectMatchesBytes(100 * 1024, 2);
    }

    @Test
    @Order(3)
    void largeBodyWithinAxumLimit() throws Exception {
        // 1.5 MiB — within axum's 2 MiB DefaultBodyLimit and the
        // 4 MiB pool cap.
        assertDirectMatchesBytes(1536 * 1024, 3);
    }

    @Test
    @Order(4)
    void overflowWithoutRetryThrowsWithExactRequiredSize() {
        byte[] body = randomBody(100 * 1024, 4);
        byte[] wire = echoWire(body);
        // Fresh thread → fresh 64 KiB pooled out buffer, guaranteed
        // smaller than the ~100 KiB wire response.
        VesperaBridge.BufferTooSmallException e = assertThrows(
                VesperaBridge.BufferTooSmallException.class,
                () -> runOnFreshThread(() ->
                        VesperaBridge.dispatchDirectPooled(wire, false)));
        assertTrue(e.requiredSize() > 100 * 1024,
                "required size must cover header + body, got " + e.requiredSize());
    }

    @Test
    @Order(5)
    void rawDispatchDirectHonoursExplicitInLen() throws Exception {
        byte[] body = randomBody(512, 5);
        byte[] wire = echoWire(body);

        // Oversized in buffer with garbage after the wire bytes —
        // explicit inLen must make the tail invisible to Rust.
        ByteBuffer in = ByteBuffer.allocateDirect(wire.length + 1024);
        in.put(wire);
        in.put(new byte[1024]); // garbage tail
        ByteBuffer out = ByteBuffer.allocateDirect(64 * 1024);

        int n = VesperaBridge.dispatchDirect(in, wire.length, out);
        assertTrue(n > 0, "expected success, got " + n);

        byte[] direct = new byte[n];
        out.get(0, direct);

        VesperaBridge.DecodedResponse viaDirect = VesperaBridge.decodeResponse(direct);
        VesperaBridge.DecodedResponse viaBytes =
                VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire));
        assertEquals(viaBytes.status(), viaDirect.status(), "status");
        assertEquals(viaBytes.body().length, viaDirect.body().length,
                "body length — a mismatch means the garbage tail leaked past inLen");
        assertArrayEquals(viaBytes.body(), viaDirect.body(), "body bytes");
        // Map equality — wire JSON key order is unspecified.
        assertEquals(viaBytes.headers(), viaDirect.headers(), "headers");
    }

    @Test
    @Order(6)
    void encodeIntoOverloadMatchesByteArrayOverload() throws Exception {
        // The encode-into overload must produce a semantically identical
        // response to the byte[]-wire overload for the same request.
        byte[] body = randomBody(100 * 1024, 6);
        Map<String, String> headers = Map.of("content-type", "application/octet-stream");

        VesperaBridge.DecodedResponse viaWire = VesperaBridge.decodeResponse(
                toArray(VesperaBridge.dispatchDirectPooled(echoWire(body), true)));
        VesperaBridge.DecodedResponse viaEncodeInto = VesperaBridge.decodeResponse(
                toArray(VesperaBridge.dispatchDirectPooled(
                        null, "POST", "/echo", null, headers, body, true)));

        assertEquals(viaWire.status(), viaEncodeInto.status(), "status");
        assertEquals(viaWire.headers(), viaEncodeInto.headers(), "headers");
        assertArrayEquals(sha256(viaWire.body()), sha256(viaEncodeInto.body()), "body");
    }

    @Test
    @Order(7)
    void microBenchmarkDirectVsBytes() throws Exception {
        System.out.println(
                "== dispatchBytes vs dispatchDirectPooled(wire) vs dispatchDirectPooled(encode-into) ==");
        Map<String, String> headers = Map.of("content-type", "application/octet-stream");
        for (int size : new int[] {1024, 64 * 1024, 1536 * 1024}) {
            byte[] body = randomBody(size, size);
            byte[] wire = echoWire(body);
            int iterations = size >= 1024 * 1024 ? 200 : 1000;

            // Warm-up all paths (JIT + pool growth).
            for (int i = 0; i < 50; i++) {
                VesperaBridge.dispatchBytes(wire);
                VesperaBridge.dispatchDirectPooled(wire, true);
                VesperaBridge.dispatchDirectPooled(null, "POST", "/echo", null, headers, body, true);
            }

            // FAIR comparison: real callers encode per request, so the
            // byte[]-based paths pay encodeRequest inside the loop too.
            long t0 = System.nanoTime();
            for (int i = 0; i < iterations; i++) {
                VesperaBridge.dispatchBytes(
                        VesperaBridge.encodeRequest(null, "POST", "/echo", null, headers, body));
            }
            long bytesNs = (System.nanoTime() - t0) / iterations;

            t0 = System.nanoTime();
            for (int i = 0; i < iterations; i++) {
                VesperaBridge.dispatchDirectPooled(
                        VesperaBridge.encodeRequest(null, "POST", "/echo", null, headers, body),
                        true);
            }
            long directNs = (System.nanoTime() - t0) / iterations;

            t0 = System.nanoTime();
            for (int i = 0; i < iterations; i++) {
                VesperaBridge.dispatchDirectPooled(null, "POST", "/echo", null, headers, body, true);
            }
            long encodeIntoNs = (System.nanoTime() - t0) / iterations;

            System.out.printf(
                    "body=%8d B  bytes=%9d ns  direct(wire)=%9d ns  direct(encodeInto)=%9d ns  "
                            + "vsBytes=%.2fx  vsWire=%.2fx%n",
                    size, bytesNs, directNs, encodeIntoNs,
                    (double) bytesNs / encodeIntoNs, (double) directNs / encodeIntoNs);
        }
    }

    /** Run on a fresh thread so the ThreadLocal pool starts at 64 KiB. */
    private static <E extends RuntimeException> void runOnFreshThread(Runnable action) throws E {
        Throwable[] thrown = new Throwable[1];
        Thread t = new Thread(() -> {
            try {
                action.run();
            } catch (Throwable e) {
                thrown[0] = e;
            }
        });
        t.start();
        try {
            t.join();
        } catch (InterruptedException ie) {
            Thread.currentThread().interrupt();
            throw new IllegalStateException(ie);
        }
        if (thrown[0] instanceof RuntimeException re) {
            throw re;
        }
        if (thrown[0] != null) {
            throw new IllegalStateException(thrown[0]);
        }
    }
}
