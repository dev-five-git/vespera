package kr.go.demo;

import com.devfive.vespera.bridge.VesperaBridge;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.MethodOrderer;
import org.junit.jupiter.api.Order;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestMethodOrder;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.Map;
import java.util.Random;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.fail;

/**
 * <strong>SIGSEGV gate</strong> for the cached
 * {@code call_method_unchecked} JNI fast path landed in
 * {@code crates/vespera_jni/src/streaming_closures.rs}.
 *
 * <p>Stress-tests the four cached Java {@code JMethodID}s the new
 * code exercises on every streaming/async dispatch:
 * <ul>
 *   <li>{@code java/io/InputStream.read([B)I} — pulled by
 *       {@link VesperaBridge#dispatchFullStreaming}</li>
 *   <li>{@code java/io/OutputStream.write([BII)V} — pushed by
 *       {@code dispatchFullStreaming} and
 *       {@code dispatchStreamingWithHeader}</li>
 *   <li>{@code java/util/function/Consumer.accept(Ljava/lang/Object;)V} —
 *       header callback fired by {@code dispatchStreamingWithHeader}
 *       before the first body byte reaches the {@code OutputStream}</li>
 *   <li>{@code java/util/concurrent/CompletableFuture.complete(Ljava/lang/Object;)Z} —
 *       async completion path used by {@code dispatchAsync}</li>
 * </ul>
 *
 * <p>If <em>any</em> of those cached method IDs resolves the wrong
 * class / signature / vtable slot, calling them through
 * {@code call_method_unchecked} will SIGSEGV the test JVM — and the
 * abnormal Gradle worker shutdown IS the test failure signal.
 * The prior E2E run never exercised the cached path because
 * {@code StreamingThroughputBenchTest} is gated behind
 * {@code -Dvespera.bench=true} (see {@code @EnabledIfSystemProperty}).
 * This test runs unconditionally as part of the normal {@code test}
 * task to lock that gap.
 *
 * <p>Verification per iteration:
 * <ul>
 *   <li>Random 1 MiB body driven by a single shared {@link Random}
 *       seed ({@code SEED}) for deterministic replay.</li>
 *   <li>SHA-256 of the body that left the JVM == SHA-256 of the body
 *       that came back through {@code /echo/stream}.</li>
 *   <li>For the bidirectional path: {@code InputStream.read} fired
 *       multiple times (multi-chunk pull) AND {@code OutputStream.write}
 *       fired multiple times (multi-chunk push), proving the cached
 *       method IDs were called repeatedly per dispatch.  With the
 *       default 64 KiB streaming chunk size and a 1 MiB payload the
 *       Rust side performs ~16 pulls + 1 EOF read and ~16 pushes
 *       per iteration.</li>
 *   <li>For the header-streaming path: {@code Consumer.accept} fires
 *       <strong>exactly once</strong> and <strong>before</strong> the
 *       first {@code OutputStream.write}; header decodes as wire JSON
 *       with status 200.</li>
 *   <li>For the async path: {@code CompletableFuture} completes
 *       successfully with a valid wire response (status 200, body
 *       matches by SHA-256).</li>
 * </ul>
 *
 * <p><strong>Iteration budget</strong> — sized to keep wall-clock for
 * the whole class comfortably under ~90s on a normal developer machine
 * while pushing the cached paths thousands of times:
 * <ul>
 *   <li>{@code dispatchFullStreaming}: {@value #BIDI_ITERATIONS} × 1 MiB
 *       → ~16 000 cached {@code InputStream.read} calls + ~16 000
 *       cached {@code OutputStream.write} calls</li>
 *   <li>{@code dispatchStreamingWithHeader}: {@value #HEADER_STREAMING_ITERATIONS}
 *       × 1 MiB → ~{@value #HEADER_STREAMING_ITERATIONS} cached
 *       {@code Consumer.accept} calls + ~8 000 cached
 *       {@code OutputStream.write} calls</li>
 *   <li>{@code dispatchAsync}: {@value #ASYNC_ITERATIONS} × 1 MiB →
 *       {@value #ASYNC_ITERATIONS} cached
 *       {@code CompletableFuture.complete} calls</li>
 * </ul>
 *
 * <p>If a slower machine pushes the run over ~90s, drop these constants
 * to 500 / 250 / 250 — the cached path is exercised plenty even at the
 * lower budget; the higher budget is just a wider net for races.
 * Per-test wall-clock is printed to stdout so reductions are
 * data-driven.
 */
@TestMethodOrder(MethodOrderer.OrderAnnotation.class)
class StreamingClosureStressTest {

    /** Shared seed so any failure replays deterministically. */
    private static final long SEED = 0xCAFEBABEL;

    /** 1 MiB — well above the default 64 KiB streaming chunk so each
     * dispatch pulls/pushes ~16 chunks, exercising the cached path
     * many times per call. */
    private static final int PAYLOAD_BYTES = 1024 * 1024;

    private static final int BIDI_ITERATIONS = 1000;
    private static final int HEADER_STREAMING_ITERATIONS = 500;
    private static final int ASYNC_ITERATIONS = 500;

    private static final Map<String, String> ECHO_HEADERS =
            Map.of("content-type", "application/octet-stream");

    /** Bound the async wait so a SIGSEGV-induced hang fails fast
     * instead of stalling the Gradle worker until its own timeout. */
    private static final long ASYNC_TIMEOUT_SECONDS = 30;

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
    }

    private static byte[] sha256(byte[] data) {
        try {
            return MessageDigest.getInstance("SHA-256").digest(data);
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-256 unavailable", e);
        }
    }

    private static byte[] randomPayload(Random rng) {
        byte[] body = new byte[PAYLOAD_BYTES];
        rng.nextBytes(body);
        return body;
    }

    /** Counts {@code read(byte[])} invocations — the exact signature
     * cached by {@code streaming_closures::call_input_stream_read}. */
    private static final class CountingInputStream extends InputStream {
        private final InputStream delegate;
        int readArrayCalls;

        CountingInputStream(InputStream delegate) {
            this.delegate = delegate;
        }

        @Override
        public int read() throws IOException {
            // Not on the cached path — but counted defensively in case
            // the Rust side ever falls back to single-byte reads.
            return delegate.read();
        }

        @Override
        public int read(byte[] b) throws IOException {
            readArrayCalls++;
            return delegate.read(b);
        }

        @Override
        public int read(byte[] b, int off, int len) throws IOException {
            // Not on the cached path — Rust calls the no-offset overload.
            return delegate.read(b, off, len);
        }
    }

    /** Counts {@code write(byte[], int, int)} invocations — the exact
     * signature cached by
     * {@code streaming_closures::call_output_stream_write}. */
    private static final class CountingByteSink extends OutputStream {
        final ByteArrayOutputStream buf = new ByteArrayOutputStream(PAYLOAD_BYTES);
        int writeRegionCalls;

        @Override
        public void write(int b) {
            // Not on the cached path; included for completeness.
            buf.write(b);
        }

        @Override
        public void write(byte[] b, int off, int len) {
            writeRegionCalls++;
            buf.write(b, off, len);
        }

        byte[] toBytes() {
            return buf.toByteArray();
        }

        int size() {
            return buf.size();
        }
    }

    /**
     * Exercises cached {@code InputStream.read([B)I} AND cached
     * {@code OutputStream.write([BII)V} repeatedly per dispatch.
     */
    @Test
    @Order(1)
    void bidirectionalStreaming_cachedReadAndWrite() throws Exception {
        Random rng = new Random(SEED);
        byte[] wireHeader = VesperaBridge.encodeRequestHeader(
                "POST", "/echo/stream", null, ECHO_HEADERS);

        long totalReads = 0;
        long totalWrites = 0;
        long t0 = System.nanoTime();

        for (int i = 0; i < BIDI_ITERATIONS; i++) {
            byte[] payload = randomPayload(rng);
            byte[] expectedSha = sha256(payload);

            CountingInputStream src = new CountingInputStream(new ByteArrayInputStream(payload));
            CountingByteSink sink = new CountingByteSink();

            byte[] respHeader =
                    VesperaBridge.dispatchFullStreaming(wireHeader, src, sink);
            VesperaBridge.DecodedResponse resp = VesperaBridge.decodeResponse(respHeader);

            assertEquals(200, resp.status(),
                    "iter " + i + ": echo must succeed (status)");
            assertEquals(PAYLOAD_BYTES, sink.size(),
                    "iter " + i + ": echoed byte count");
            assertArrayEquals(expectedSha, sha256(sink.toBytes()),
                    "iter " + i + ": SHA-256 round-trip");
            assertTrue(src.readArrayCalls > 1,
                    "iter " + i + ": expected multi-chunk pulls through cached"
                            + " InputStream.read, got " + src.readArrayCalls);
            assertTrue(sink.writeRegionCalls > 1,
                    "iter " + i + ": expected multi-chunk pushes through cached"
                            + " OutputStream.write, got " + sink.writeRegionCalls);

            totalReads += src.readArrayCalls;
            totalWrites += sink.writeRegionCalls;
        }

        long elapsedMs = (System.nanoTime() - t0) / 1_000_000L;
        System.out.printf(
                "STRESS bidi(/echo/stream): iter=%d payload=%dB elapsed=%dms"
                        + " cachedReads=%d cachedWrites=%d (avg/iter %.1f reads, %.1f writes)%n",
                BIDI_ITERATIONS, PAYLOAD_BYTES, elapsedMs,
                totalReads, totalWrites,
                (double) totalReads / BIDI_ITERATIONS,
                (double) totalWrites / BIDI_ITERATIONS);
    }

    /**
     * Exercises cached {@code Consumer.accept(Ljava/lang/Object;)V}
     * (once per dispatch, before any body byte) and cached
     * {@code OutputStream.write([BII)V} (many times per dispatch).
     */
    @Test
    @Order(2)
    void responseStreamingWithHeader_cachedConsumerAndWrite() throws Exception {
        Random rng = new Random(SEED);
        long totalHeaderCalls = 0;
        long totalWrites = 0;
        long t0 = System.nanoTime();

        for (int i = 0; i < HEADER_STREAMING_ITERATIONS; i++) {
            byte[] payload = randomPayload(rng);
            byte[] expectedSha = sha256(payload);

            byte[] wireRequest = VesperaBridge.encodeRequest(
                    "POST", "/echo/stream", null, ECHO_HEADERS, payload);

            CountingByteSink sink = new CountingByteSink();
            AtomicInteger headerCalls = new AtomicInteger();
            AtomicReference<byte[]> headerBytesRef = new AtomicReference<>();
            // -1 sentinel; captured value MUST be 0 (no writes yet when
            // the header consumer is called).
            AtomicLong writesAtHeaderTime = new AtomicLong(-1);

            VesperaBridge.dispatchStreamingWithHeader(
                    wireRequest,
                    headerBytes -> {
                        writesAtHeaderTime.set(sink.writeRegionCalls);
                        // Copy because the JNI side may reuse the array.
                        headerBytesRef.set(headerBytes.clone());
                        headerCalls.incrementAndGet();
                    },
                    sink);

            assertEquals(1, headerCalls.get(),
                    "iter " + i + ": header consumer must fire exactly once");
            assertEquals(0L, writesAtHeaderTime.get(),
                    "iter " + i + ": header consumer must fire BEFORE any"
                            + " OutputStream.write");
            byte[] hdr = headerBytesRef.get();
            assertNotNull(hdr, "iter " + i + ": header bytes captured");

            VesperaBridge.DecodedResponse resp = VesperaBridge.decodeResponse(hdr);
            assertEquals(200, resp.status(),
                    "iter " + i + ": wire header parses with status 200");
            assertEquals(PAYLOAD_BYTES, sink.size(),
                    "iter " + i + ": echoed byte count");
            assertArrayEquals(expectedSha, sha256(sink.toBytes()),
                    "iter " + i + ": SHA-256 round-trip");
            assertTrue(sink.writeRegionCalls > 1,
                    "iter " + i + ": expected multi-chunk pushes through cached"
                            + " OutputStream.write, got " + sink.writeRegionCalls);

            totalHeaderCalls += headerCalls.get();
            totalWrites += sink.writeRegionCalls;
        }

        long elapsedMs = (System.nanoTime() - t0) / 1_000_000L;
        System.out.printf(
                "STRESS header-stream(/echo/stream): iter=%d payload=%dB elapsed=%dms"
                        + " cachedConsumerCalls=%d cachedWrites=%d (avg/iter %.1f writes)%n",
                HEADER_STREAMING_ITERATIONS, PAYLOAD_BYTES, elapsedMs,
                totalHeaderCalls, totalWrites,
                (double) totalWrites / HEADER_STREAMING_ITERATIONS);
    }

    /**
     * Exercises cached
     * {@code CompletableFuture.complete(Ljava/lang/Object;)Z}.
     */
    @Test
    @Order(3)
    void asyncDispatch_cachedFutureComplete() throws Exception {
        Random rng = new Random(SEED);
        long t0 = System.nanoTime();

        for (int i = 0; i < ASYNC_ITERATIONS; i++) {
            byte[] payload = randomPayload(rng);
            byte[] expectedSha = sha256(payload);

            byte[] wireRequest = VesperaBridge.encodeRequest(
                    "POST", "/echo/stream", null, ECHO_HEADERS, payload);

            CompletableFuture<byte[]> future = new CompletableFuture<>();
            VesperaBridge.dispatchAsync(future, wireRequest);

            byte[] wireResponse;
            try {
                wireResponse = future.get(ASYNC_TIMEOUT_SECONDS, TimeUnit.SECONDS);
            } catch (TimeoutException te) {
                fail("iter " + i + ": dispatchAsync future did not complete within "
                        + ASYNC_TIMEOUT_SECONDS + "s");
                return; // unreachable; keeps the compiler happy
            }

            assertNotNull(wireResponse,
                    "iter " + i + ": future must complete with non-null payload");
            assertTrue(future.isDone() && !future.isCompletedExceptionally(),
                    "iter " + i + ": future must be normally completed");

            VesperaBridge.DecodedResponse resp = VesperaBridge.decodeResponse(wireResponse);
            assertEquals(200, resp.status(), "iter " + i + ": status");
            assertEquals(PAYLOAD_BYTES, resp.body().remaining(),
                    "iter " + i + ": body length");
            assertArrayEquals(expectedSha, sha256(resp.bodyBytes()),
                    "iter " + i + ": SHA-256 round-trip");
        }

        long elapsedMs = (System.nanoTime() - t0) / 1_000_000L;
        System.out.printf(
                "STRESS async(/echo/stream): iter=%d payload=%dB elapsed=%dms"
                        + " cachedFutureCompleteCalls=%d%n",
                ASYNC_ITERATIONS, PAYLOAD_BYTES, elapsedMs, ASYNC_ITERATIONS);
    }
}
