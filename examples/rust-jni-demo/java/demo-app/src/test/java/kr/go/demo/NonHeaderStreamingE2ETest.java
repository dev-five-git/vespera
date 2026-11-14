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
                "POST", "/diag/panic-mid-body/512", null, Map.of(), new byte[0]);
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        VesperaStreamAbortedException aborted = assertThrows(VesperaStreamAbortedException.class,
                () -> VesperaBridge.dispatchStreaming(request, out));
        assertTrue(aborted.panicked(), aborted.getMessage());
        assertTrue(out.size() > 0, "body bytes were written before the panic");
    }

    @Test
    void fullStreamingPanicAfterBodyBytesThrowsAPanickedAbort() {
        byte[] header = VesperaBridge.encodeRequestHeader(
                "POST", "/diag/panic-mid-body/512", null, Map.of());
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
        // Retry while other dispatches change the in-flight count around us.
        for (int attempt = 0; attempt < 20; attempt++) {
            int before = asyncInFlight();
            CompletableFuture<byte[]> future = new CompletableFuture<>();
            AtomicReference<Integer> seenInline = new AtomicReference<>();
            // Registered BEFORE the dispatch: runs inline on the completing
            // (= calling) thread when the request is rejected on-thread.
            future.thenAccept(ignored -> seenInline.set(asyncInFlight()));
            // A null request is rejected on the calling thread (400) after
            // the in-flight permit was acquired.
            VesperaBridge.dispatchAsync(future, null);
            assertEquals(400, VesperaBridge.decodeResponse(future.get(30, TimeUnit.SECONDS)).status());
            int after = asyncInFlight();
            if (before == after) {
                assertEquals(before, seenInline.get(),
                        "the rejected dispatch's own slot is free when its continuation runs");
                return;
            }
        }
        throw new AssertionError("async in-flight count never settled");
    }
}
