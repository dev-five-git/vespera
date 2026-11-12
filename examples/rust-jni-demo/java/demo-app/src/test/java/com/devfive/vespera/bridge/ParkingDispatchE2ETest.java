package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

/**
 * {@code AsyncDispatch.dispatchPollFirst0} — the native half of
 * {@link ParkingDispatch} — against the real {@code rust_jni_demo} cdylib:
 * a handler that completes in the first poll answers inline (future
 * untouched), one that waits on a timer is handed to a Tokio worker that
 * completes the future, and a panicking handler answers {@code 500} inline.
 * Plain threads suffice: the strategy does not depend on the caller being a
 * virtual thread (the JDK 17 CI legs run it too).
 */
class ParkingDispatchE2ETest {

    @BeforeAll
    static void loadNative() {
        VesperaBridge.init("rust_jni_demo");
    }

    private static byte[] get(String path) {
        return VesperaBridge.encodeRequest(null, "GET", path, null, Map.of(), null);
    }

    private static String body(VesperaBridge.DecodedResponse r) {
        return new String(r.bodyBytes(), StandardCharsets.UTF_8);
    }

    @Test
    void readyHandlerAnswersInlineWithoutTouchingTheFuture() {
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        byte[] wire = AsyncDispatch.dispatchPollFirst0(future, get("/health"));
        assertNotNull(wire, "a handler that never waits completes in the first poll");
        VesperaBridge.DecodedResponse r = VesperaBridge.decodeResponse(wire);
        assertEquals(200, r.status());
        assertEquals("ok", body(r));
        assertFalse(future.isDone());
    }

    @Test
    void waitingHandlerIsSpawnedAndCompletesTheFuture() throws Exception {
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        assertNull(AsyncDispatch.dispatchPollFirst0(future, get("/diag/sleep/pollfirst-e2e/30")),
                "a handler waiting on a timer is still pending after the first poll");
        VesperaBridge.DecodedResponse r = VesperaBridge.decodeResponse(future.get(30, TimeUnit.SECONDS));
        assertEquals(200, r.status());
        assertEquals("slept", body(r));
    }

    @Test
    void panicInTheFirstPollAnswers500Inline() {
        byte[] request = VesperaBridge.encodeRequest(null, "POST", "/echo/panic", null, Map.of(), new byte[0]);
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        byte[] wire = AsyncDispatch.dispatchPollFirst0(future, request);
        assertNotNull(wire);
        VesperaBridge.DecodedResponse r = VesperaBridge.decodeResponse(wire);
        assertEquals(500, r.status());
        assertEquals("true", r.metadata().get("panicked"), r.metadata().toString());
        assertFalse(future.isDone());
    }

    @Test
    void invalidWireAnswersInline() {
        byte[] wire = AsyncDispatch.dispatchPollFirst0(new CompletableFuture<>(), new byte[] {0, 0, 0, 9, 1});
        assertNotNull(wire);
        assertEquals(400, VesperaBridge.decodeResponse(wire).status());
    }

    @Test
    void parkMixesInlineAndSpawnedDispatchesUnderConcurrency() throws Exception {
        List<CompletableFuture<String>> results = new ArrayList<>();
        for (int i = 0; i < 64; i++) {
            String path = i % 2 == 0 ? "/health" : "/diag/sleep/pollfirst-mix/5";
            results.add(CompletableFuture.supplyAsync(
                    () -> body(VesperaBridge.decodeResponse(ParkingDispatch.park(get(path))))));
        }
        for (int i = 0; i < results.size(); i++) {
            String expected = i % 2 == 0 ? "ok" : "slept";
            assertEquals(expected, results.get(i).get(30, TimeUnit.SECONDS));
        }
        assertTrue(VesperaBridge.isInitialized());
    }
}
