package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import java.util.function.LongPredicate;
import org.junit.jupiter.api.Test;

/** Cancellation wiring of {@link AsyncDispatch} (no native library needed). */
class AsyncDispatchTest {

    private static final long TOKEN = 42L;

    private final List<Long> cancelled = new CopyOnWriteArrayList<>();
    private final LongPredicate canceller = token -> cancelled.add(token);

    @Test
    void cancellingTheFutureCancelsTheNativeTask() {
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        AsyncDispatch.linkCancellation(future, TOKEN, canceller);
        assertTrue(cancelled.isEmpty());
        future.cancel(true);
        assertEquals(List.of(TOKEN), cancelled);
        // Cancelling again does not reach native twice.
        future.cancel(true);
        assertEquals(List.of(TOKEN), cancelled);
    }

    @Test
    void anyExceptionalCompletionCancelsTheNativeTask() throws Exception {
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        AsyncDispatch.linkCancellation(future, TOKEN, canceller);
        future.orTimeout(1, TimeUnit.MILLISECONDS);
        assertThrows(Exception.class, future::join);
        assertEquals(List.of(TOKEN), cancelled);

        CompletableFuture<byte[]> failed = new CompletableFuture<>();
        AsyncDispatch.linkCancellation(failed, TOKEN + 1, canceller);
        failed.completeExceptionally(new TimeoutException("caller gave up"));
        assertEquals(List.of(TOKEN, TOKEN + 1), cancelled);
    }

    @Test
    void normalCompletionNeverCancels() {
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        AsyncDispatch.linkCancellation(future, TOKEN, canceller);
        future.complete(new byte[] {1});
        future.cancel(true); // too late: already completed with the native result
        assertTrue(cancelled.isEmpty(), cancelled.toString());
    }

    @Test
    void zeroTokenOrDoneFutureLinksNothing() {
        CompletableFuture<byte[]> notSpawned = new CompletableFuture<>();
        AsyncDispatch.linkCancellation(notSpawned, 0L, canceller);
        notSpawned.cancel(true);

        CompletableFuture<byte[]> done = CompletableFuture.completedFuture(new byte[0]);
        AsyncDispatch.linkCancellation(done, TOKEN, canceller);
        assertTrue(cancelled.isEmpty(), cancelled.toString());
    }

    @Test
    void cancelBeforeLinkingStillReachesNative() {
        // The future may be cancelled between the native spawn and the link.
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        future.cancel(false);
        AsyncDispatch.linkCancellation(future, TOKEN, canceller);
        assertEquals(List.of(TOKEN), cancelled);
    }

    @Test
    void withoutTheNativeLibraryTheSymbolsAreMissing() {
        assertThrows(UnsatisfiedLinkError.class,
                () -> AsyncDispatch.dispatch(new CompletableFuture<>(), new byte[4]));
        assertThrows(UnsatisfiedLinkError.class, () -> AsyncDispatch.cancel0(TOKEN));
        assertThrows(UnsatisfiedLinkError.class,
                () -> VesperaBridge.dispatchAsync(new CompletableFuture<>(), new byte[4]));
    }
}
