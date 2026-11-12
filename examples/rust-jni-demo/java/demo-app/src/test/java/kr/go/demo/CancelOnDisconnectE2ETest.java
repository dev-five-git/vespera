package kr.go.demo;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.devfive.vespera.bridge.VesperaBridge;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.CancellationException;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.function.BooleanSupplier;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

/**
 * Cancel-on-disconnect, straight through the JNI symbols:
 *
 * <ul>
 *   <li>streaming modes: when the servlet {@code OutputStream} write fails
 *       (the client disconnected), the Rust side stops polling and drops the
 *       response body at once (the {@code /diag/drip} stream would otherwise
 *       trickle for minutes);</li>
 *   <li>{@code dispatchAsync}: cancelling the {@code CompletableFuture}
 *       drops the running handler and hands its in-flight slot back.</li>
 * </ul>
 *
 * The routes record {@code "<key>:dropped"} from a drop guard, read back via
 * {@code /diag/runs/<key>:dropped}.
 */
class CancelOnDisconnectE2ETest {

    /** 10 000 chunks, one every 20 ms: ~200 s if never cancelled. */
    private static final String DRIP = "/diag/drip/%s/10000/20";

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
    }

    private static String get(String path) {
        byte[] wire = VesperaBridge.encodeRequest("GET", path, null, Map.of(), null);
        VesperaBridge.DecodedResponse response =
                VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire));
        assertEquals(200, response.status(), path);
        return new String(response.bodyBytes(), StandardCharsets.UTF_8);
    }

    private static long runs(String key) {
        return Long.parseLong(get("/diag/runs/" + key));
    }

    private static long asyncInFlight() {
        return Long.parseLong(get("/diag/async-in-flight"));
    }

    /**
     * Waits until no {@code dispatchAsync} holds an in-flight slot and returns
     * that baseline ({@code 0}).  The counter is process-wide: async dispatches
     * of earlier tests (in this class or others sharing the JVM) may still be
     * winding down — e.g. a cancelled task releases its slot only once a
     * Tokio worker polls it again — and reading a transient value as the
     * baseline would make the exact {@code baseline + 1} check below miss.
     */
    private static long awaitAsyncIdle() throws InterruptedException {
        await("no async dispatch in flight", () -> asyncInFlight() == 0);
        return 0;
    }

    /** Waits until the {@code /diag/sleep} handler of {@code key} is running. */
    private static void awaitStarted(String key) throws InterruptedException {
        await(key + " started", () -> runs(key + ":started") == 1);
    }

    private static void await(String what, BooleanSupplier condition) throws InterruptedException {
        // Generous: CI runners (macOS / Windows) can be slow to schedule.
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(30);
        while (!condition.getAsBoolean()) {
            if (System.nanoTime() > deadline) {
                throw new AssertionError("timed out waiting for: " + what
                        + " (async in flight: " + asyncInFlight() + ")");
            }
            Thread.sleep(10);
        }
    }

    /** Accepts {@code okWrites} body writes, then throws like a dead socket. */
    private static final class DisconnectingStream extends OutputStream {
        private final int okWrites;
        private int writes;

        DisconnectingStream(int okWrites) {
            this.okWrites = okWrites;
        }

        @Override
        public void write(int b) throws IOException {
            write(new byte[] {(byte) b}, 0, 1);
        }

        @Override
        public void write(byte[] b, int off, int len) throws IOException {
            if (++writes > okWrites) {
                throw new IOException("Broken pipe (client disconnected)");
            }
        }

        @Override
        public void flush() throws IOException {
            if (writes >= okWrites) {
                throw new IOException("Broken pipe (client disconnected)");
            }
        }
    }

    private static void assertDroppedPromptly(String key, long startNanos) throws Exception {
        await(key + " dropped", () -> runs(key + ":dropped") == 1);
        long elapsedMillis = TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - startNanos);
        assertTrue(elapsedMillis < 5_000, "dropped after " + elapsedMillis + " ms");
        assertEquals(0, runs(key), "the stream never ran to its end");
    }

    @Test
    void headerFirstStreamingDropsTheBodyWhenTheClientWriteFails() throws Exception {
        String key = "cancel-stream-header-" + System.nanoTime();
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", String.format(DRIP, key), null, Map.of(), null);
        long start = System.nanoTime();
        try {
            VesperaBridge.dispatchStreamingWithHeader(wire, header -> { }, new DisconnectingStream(2));
        } catch (Exception expected) {
            // The truncated stream surfaces as an IOException thrown from native.
        }
        assertDroppedPromptly(key, start);
    }

    @Test
    void headerLastStreamingDropsTheBodyWhenTheClientWriteFails() throws Exception {
        String key = "cancel-stream-last-" + System.nanoTime();
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", String.format(DRIP, key), null, Map.of(), null);
        long start = System.nanoTime();
        // A sink failure after body bytes were written surfaces as IOException.
        assertThrows(IOException.class,
                () -> VesperaBridge.dispatchStreaming(wire, new DisconnectingStream(2)));
        assertDroppedPromptly(key, start);
    }

    @Test
    void bidirectionalStreamingDropsTheBodyWhenTheClientWriteFails() throws Exception {
        String key = "cancel-stream-bidi-" + System.nanoTime();
        byte[] header = VesperaBridge.encodeRequestHeader(
                "GET", String.format(DRIP, key), null, Map.<String, String>of());
        long start = System.nanoTime();
        try {
            VesperaBridge.dispatchFullStreamingWithHeader(
                    header, h -> { }, new ByteArrayInputStream(new byte[0]), new DisconnectingStream(2));
        } catch (Exception expected) {
            // See headerFirstStreamingDropsTheBodyWhenTheClientWriteFails.
        }
        assertDroppedPromptly(key, start);
    }

    @Test
    void cancellingTheAsyncFutureAbortsTheHandlerAndFreesItsSlot() throws Exception {
        String key = "cancel-async-" + System.nanoTime();
        long baseline = awaitAsyncIdle();
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", "/diag/sleep/" + key + "/60000", null, Map.of(), null);
        CompletableFuture<byte[]> future = VesperaBridge.dispatch(wire);
        await("async dispatch admitted", () -> asyncInFlight() == baseline + 1);
        // The slot is taken at admission, before a Tokio worker first polls
        // the task: a cancel in that window drops the request before the
        // handler (and its drop probe) exists, so no ":dropped" is ever
        // recorded.  Cancel only once the handler is really sleeping.
        awaitStarted(key);
        assertFalse(future.isDone());

        long start = System.nanoTime();
        assertTrue(future.cancel(true));
        await(key + " dropped", () -> runs(key + ":dropped") == 1);
        await("in-flight slot released", () -> asyncInFlight() == baseline);
        long elapsedMillis = TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - start);
        assertTrue(elapsedMillis < 5_000, "cancelled after " + elapsedMillis + " ms");
        assertEquals(0, runs(key), "the 60 s handler never finished");
        assertTrue(future.isCancelled(), "the late native completion is a no-op");
        assertThrows(CancellationException.class, future::join);
    }

    @Test
    void anExternalTimeoutAlsoAbortsTheAsyncHandler() throws Exception {
        String key = "cancel-async-timeout-" + System.nanoTime();
        long baseline = awaitAsyncIdle();
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", "/diag/sleep/" + key + "/60000", null, Map.of(), null);
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        VesperaBridge.dispatchAsync(future, wire);
        // Same race as above: time out only a handler that already runs.
        awaitStarted(key);
        future.orTimeout(100, TimeUnit.MILLISECONDS);
        await(key + " dropped", () -> runs(key + ":dropped") == 1);
        await("in-flight slot released", () -> asyncInFlight() == baseline);
        assertTrue(future.isCompletedExceptionally());
    }

    @Test
    void anAsyncDispatchThatFinishesIsNotCancelled() throws Exception {
        String key = "cancel-async-done-" + System.nanoTime();
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", "/diag/sleep/" + key + "/1", null, Map.of(), null);
        CompletableFuture<byte[]> future = VesperaBridge.dispatch(wire);
        byte[] response = future.get(10, TimeUnit.SECONDS);
        assertEquals(200, VesperaBridge.decodeResponse(response).status());
        assertFalse(future.cancel(true), "already completed");
        assertEquals(1, runs(key));
        assertEquals(0, runs(key + ":dropped"));
    }
}
