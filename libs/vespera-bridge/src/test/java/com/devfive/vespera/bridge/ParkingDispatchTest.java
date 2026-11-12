package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.concurrent.CompletableFuture;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.Test;

class ParkingDispatchTest {

    /** Without a native library both routes reach their (missing) native symbol. */
    @Test
    void virtualThreadsTakeThePollFirstRoutePlatformThreadsTheSyncOne() {
        UnsatisfiedLinkError parked = assertThrows(UnsatisfiedLinkError.class,
                () -> ParkingDispatch.dispatchBytes(new byte[4], true));
        assertTrue(parked.getMessage().contains("AsyncDispatch.dispatchPollFirst0"), parked.getMessage());
        UnsatisfiedLinkError sync = assertThrows(UnsatisfiedLinkError.class,
                () -> ParkingDispatch.dispatchBytes(new byte[4], false));
        assertTrue(sync.getMessage().contains("dispatchBytes"), sync.getMessage());
    }

    @Test
    void pollFirstOffFallsBackToDispatchAsync() {
        UnsatisfiedLinkError async = assertThrows(UnsatisfiedLinkError.class,
                () -> ParkingDispatch.park(new byte[4], false, (f, w) -> {
                    throw new AssertionError("poll-first must not run");
                }));
        assertTrue(async.getMessage().contains("AsyncDispatch.dispatch0"), async.getMessage());
    }

    @Test
    void inlineResultIsReturnedWithoutWaitingOnTheFuture() {
        byte[] wire = {1, 2, 3, 4};
        byte[] response = {9};
        AtomicReference<CompletableFuture<byte[]>> seen = new AtomicReference<>();
        byte[] got = ParkingDispatch.park(wire, true, (future, request) -> {
            assertSame(wire, request);
            seen.set(future);
            return response;
        });
        assertSame(response, got);
        assertNotNull(seen.get());
        assertFalse(seen.get().isDone(), "an inline result leaves the future untouched");
    }

    @Test
    void pendingDispatchParksUntilTheFutureCompletes() {
        byte[] response = {7, 7};
        byte[] got = ParkingDispatch.park(new byte[4], true, (future, request) -> {
            Thread worker = new Thread(() -> {
                try {
                    Thread.sleep(20);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
                future.complete(response);
            });
            worker.setDaemon(true);
            worker.start();
            return null;
        });
        assertArrayEquals(response, got);
    }

    @Test
    void pollFirstIsOnUnlessExplicitlyFalse() {
        assertTrue(ParkingDispatch.pollFirstEnabled(null));
        assertTrue(ParkingDispatch.pollFirstEnabled("true"));
        assertTrue(ParkingDispatch.pollFirstEnabled(""));
        assertFalse(ParkingDispatch.pollFirstEnabled("false"));
        assertFalse(ParkingDispatch.pollFirstEnabled(" FALSE "));
    }
}
