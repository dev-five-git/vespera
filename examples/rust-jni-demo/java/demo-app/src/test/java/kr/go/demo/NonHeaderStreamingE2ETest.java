package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import com.devfive.vespera.bridge.VesperaStreamAbortedException;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

/**
 * The non-header streaming symbols ({@code dispatchStreaming},
 * {@code dispatchFullStreaming}) return the response header only after the
 * body went to the caller's {@code OutputStream}, and {@code dispatchAsync}'s
 * on-thread completions, against the real {@code rust_jni_demo} cdylib:
 * <ul>
 *   <li>the header-first dispatcher's empty flush hints never reach the
 *       caller's stream as a {@code flush()} (that would commit a servlet
 *       response before its status is known);</li>
 *   <li>a Rust panic after body bytes were written throws
 *       {@link VesperaStreamAbortedException} ({@code panicked()}) instead of
 *       returning a {@code 500} header behind a partial body;</li>
 *   <li>an on-thread {@code dispatchAsync} completion has already released
 *       its in-flight slot when the future's inline continuations run.</li>
 * </ul>
 */
class NonHeaderStreamingE2ETest {

    /**
     * 1 KiB response chunks the {@code /diag/panic-mid-body} handler must emit
     * before panicking so that at least one batch reaches the sink first: two
     * streaming chunks' worth (the Rust side batches ready frames up to
     * the effective streaming chunk size), at least 512. The chunk size is
     * resolved exactly like the bridge + Rust do it:
     * <ol>
     *   <li>the {@code vespera.streaming.chunkBytes} system property (which
     *       the build forwards into this JVM), parsed like
     *       {@code VesperaNativeConfig.longProperty} (decimal, or {@code 0x} /
     *       {@code #} hex, so {@code 0x100000} counts and a leading zero is
     *       decimal: see {@link #parseBridgeInteger}); a malformed value or one {@code <= 0}
     *       is never forwarded ({@code configureStreaming0} leaves the
     *       setting untouched), so it falls through to</li>
     *   <li>the {@code VESPERA_STREAMING_CHUNK_BYTES} environment variable the
     *       native side reads (a non-negative decimal integer; anything else
     *       means the default), else</li>
     *   <li>256 KiB;</li>
     * </ol>
     * clamped to 4 KiB to 8 MiB. (The handler caps the count at 10 000, still
     * past one 8 MiB chunk.)
     */
    static int panicChunksPastOneStreamingChunk() {
        return panicChunksPastOneStreamingChunk(
                System.getProperty("vespera.streaming.chunkBytes"),
                System.getenv("VESPERA_STREAMING_CHUNK_BYTES"));
    }

    static int panicChunksPastOneStreamingChunk(String property, String env) {
        return (int) Math.max(512, 2L * effectiveStreamingChunkBytes(property, env) / 1024);
    }

    /**
     * The streaming chunk size the native side uses in this JVM, resolved
     * like {@link #panicChunksPastOneStreamingChunk()} documents (also used by
     * {@code StreamingClosureStressTest} to size its payload).
     */
    static int effectiveStreamingChunkBytes() {
        return effectiveStreamingChunkBytes(
                System.getProperty("vespera.streaming.chunkBytes"),
                System.getenv("VESPERA_STREAMING_CHUNK_BYTES"));
    }

    static int effectiveStreamingChunkBytes(String property, String env) {
        long chunkBytes = forwardedChunkBytes(property);
        if (chunkBytes <= 0) {
            chunkBytes = envChunkBytes(env);
        }
        return (int) Math.max(4 * 1024, Math.min(8L * 1024 * 1024, chunkBytes));
    }

    /**
     * The bridge's rule for an integer system property
     * ({@code VesperaNativeConfig.parseIntegerProperty}): an optional sign, then
     * {@code 0x} / {@code 0X} / {@code #} hexadecimal, else decimal. A leading zero
     * is decimal, unlike {@code Long.decode}, which reads {@code 01000000} as octal
     * and rejects {@code 0524288}. Malformed input throws NumberFormatException.
     *
     * <p>This is a copy kept for the E2E assertions; the source of truth is
     * {@code VesperaNativeConfigParseTest} in {@code libs/vespera-bridge}, which
     * pins the real {@code VesperaNativeConfig.parseIntegerProperty}. Keep the
     * two in step.
     */
    static long parseBridgeInteger(String text) {
        String sign = "";
        String digits = text;
        if (digits.startsWith("-") || digits.startsWith("+")) {
            sign = digits.substring(0, 1);
            digits = digits.substring(1);
        }
        int radix = 10;
        if (digits.startsWith("0x") || digits.startsWith("0X")) {
            radix = 16;
            digits = digits.substring(2);
        } else if (digits.startsWith("#")) {
            radix = 16;
            digits = digits.substring(1);
        }
        if (digits.startsWith("-") || digits.startsWith("+")) {
            throw new NumberFormatException("sign after the radix prefix: " + text);
        }
        return Long.parseLong(sign + digits, radix);
    }

    /** What the bridge forwards to {@code configureStreaming0} ({@code <= 0} = nothing). */
    private static long forwardedChunkBytes(String property) {
        if (property == null) {
            return 0;
        }
        try {
            // Saturated to an int like VesperaNativeConfig.toInt.
            return Math.min(Integer.MAX_VALUE, parseBridgeInteger(property.trim()));
        } catch (NumberFormatException malformed) {
            return 0;
        }
    }

    /** {@code VESPERA_STREAMING_CHUNK_BYTES} as Rust reads it ({@code usize} parse, else the default). */
    private static long envChunkBytes(String env) {
        long fallback = 256 * 1024;
        if (env == null) {
            return fallback;
        }
        try {
            long value = Long.parseLong(env.trim());
            return value >= 0 ? value : fallback;
        } catch (NumberFormatException malformed) {
            return fallback;
        }
    }

    @Test
    void panicChunkSizingFollowsTheNativeChunkSizePrecedence() {
        assertEquals(512, panicChunksPastOneStreamingChunk(null, null));
        assertEquals(16384, panicChunksPastOneStreamingChunk(null, "8388608"));
        assertEquals(1024, panicChunksPastOneStreamingChunk("524288", "8388608"));
        assertEquals(16384, panicChunksPastOneStreamingChunk(null, " 99999999 "));
        assertEquals(512, panicChunksPastOneStreamingChunk(null, "big"));
        // The bridge reads the property as decimal or 0x/#-hex: hex counts.
        assertEquals(2048, panicChunksPastOneStreamingChunk("0x100000", null));
        // A leading zero is decimal in the bridge (Long.decode would read 01000000 as octal,
        // and reject 0524288 outright).
        assertEquals(1953, panicChunksPastOneStreamingChunk("01000000", null));
        assertEquals(1000000, effectiveStreamingChunkBytes("01000000", null));
        assertEquals(1024, panicChunksPastOneStreamingChunk("0524288", null));
        // A property that is never forwarded (<= 0 or malformed) leaves the env var in charge.
        assertEquals(16384, panicChunksPastOneStreamingChunk("0", "8388608"));
        assertEquals(16384, panicChunksPastOneStreamingChunk("-1", "8388608"));
        assertEquals(16384, panicChunksPastOneStreamingChunk("64MiB", "8388608"));
        // Rust: a negative env value is invalid (the default), 0 clamps to 4 KiB.
        assertEquals(512, panicChunksPastOneStreamingChunk(null, "-5"));
        assertEquals(512, panicChunksPastOneStreamingChunk(null, "0"));
        assertEquals(256 * 1024, effectiveStreamingChunkBytes(null, null));
        assertEquals(4 * 1024, effectiveStreamingChunkBytes("1", null));
        assertEquals(8 * 1024 * 1024, effectiveStreamingChunkBytes(null, "99999999"));
        assertEquals(1024 * 1024, effectiveStreamingChunkBytes("1048576", null));
    }

    static final String PANIC_PAST_ONE_CHUNK =
            "/diag/panic-mid-body/" + panicChunksPastOneStreamingChunk();

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
    }

    /** Records writes and flushes. */
    private static final class RecordingOutputStream extends ByteArrayOutputStream {
        int flushes;

        @Override
        public void flush() {
            flushes++;
        }
    }

    @Test
    void fullStreamingNeverFlushesTheCallersStreamMidResponse() throws Exception {
        // Three 1 KiB chunks 30 ms apart: the body waits between them, which
        // is when the header-first dispatcher emits its flush hints.
        byte[] header = VesperaBridge.encodeRequestHeader(
                "GET", "/diag/drip/nonheader-flush/3/30", null, Map.of());
        RecordingOutputStream out = new RecordingOutputStream();
        byte[] response = VesperaBridge.dispatchFullStreaming(
                header, new ByteArrayInputStream(new byte[0]), out);
        assertEquals(200, VesperaBridge.decodeResponse(response).status());
        assertEquals(3 * 1024, out.size());
        assertEquals(0, out.flushes, "a flush hint must not flush the caller's stream");
    }

    @Test
    void responseStreamingPanicAfterBodyBytesThrowsAPanickedAbort() {
        byte[] request = VesperaBridge.encodeRequest(
                "POST", PANIC_PAST_ONE_CHUNK, null, Map.of(), new byte[0]);
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        VesperaStreamAbortedException aborted = assertThrows(VesperaStreamAbortedException.class,
                () -> VesperaBridge.dispatchStreaming(request, out));
        assertTrue(aborted.panicked(), aborted.getMessage());
        assertTrue(out.size() > 0, "body bytes were written before the panic");
    }

    @Test
    void fullStreamingPanicAfterBodyBytesThrowsAPanickedAbort() {
        byte[] header = VesperaBridge.encodeRequestHeader(
                "POST", PANIC_PAST_ONE_CHUNK, null, Map.of());
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        VesperaStreamAbortedException aborted = assertThrows(VesperaStreamAbortedException.class,
                () -> VesperaBridge.dispatchFullStreaming(
                        header, new ByteArrayInputStream(new byte[0]), out));
        assertTrue(aborted.panicked(), aborted.getMessage());
        assertTrue(out.size() > 0, "body bytes were written before the panic");
    }

    @Test
    void panicBeforeAnyBodyByteIsStillA500Header() throws Exception {
        byte[] request = VesperaBridge.encodeRequest(
                "POST", "/diag/panic-mid-body/0", null, Map.of(), new byte[0]);
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        VesperaBridge.DecodedResponse r =
                VesperaBridge.decodeResponse(VesperaBridge.dispatchStreaming(request, out));
        assertEquals(500, r.status());
        assertEquals("true", r.metadata().get("panicked"), r.metadata().toString());
        assertEquals(0, out.size());
    }

    private static int asyncInFlight() {
        byte[] wire = VesperaBridge.dispatchBytes(VesperaBridge.encodeRequest(
                "GET", "/diag/async-in-flight", null, Map.of(), null));
        return Integer.parseInt(
                new String(VesperaBridge.decodeResponse(wire).bodyBytes(), StandardCharsets.UTF_8));
    }

    @Test
    void onThreadAsyncCompletionReleasesItsSlotBeforeContinuationsRun() throws Exception {
        // Stale slots from earlier cancelled or timed-out async dispatches (see
        // CancelOnDisconnectE2ETest.awaitAsyncIdle) can release late and move the
        // count inside the window below, so wait for the process to go idle first.
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(30);
        while (asyncInFlight() != 0) {
            if (System.nanoTime() > deadline) {
                throw new AssertionError("async in-flight count never settled");
            }
            Thread.sleep(10);
        }
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        AtomicReference<Integer> seenInline = new AtomicReference<>();
        // Registered BEFORE the dispatch: runs inline on the completing
        // (= calling) thread when the request is rejected on-thread.
        future.thenAccept(ignored -> seenInline.set(asyncInFlight()));
        // A null request is rejected on the calling thread (400) after
        // the in-flight permit was acquired.
        VesperaBridge.dispatchAsync(future, null);
        assertEquals(400, VesperaBridge.decodeResponse(future.get(30, TimeUnit.SECONDS)).status());
        assertEquals(0, seenInline.get(),
                "the rejected dispatch's own slot is free when its continuation runs");
    }
}
