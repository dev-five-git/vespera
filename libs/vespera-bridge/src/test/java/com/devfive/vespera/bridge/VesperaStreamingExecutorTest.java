package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.time.Duration;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.Test;

/** {@link VesperaStreamingExecutor}: admission (threads + queue), slot release, close. */
class VesperaStreamingExecutorTest {

    private static void awaitIdle(VesperaStreamingExecutor executor) throws InterruptedException {
        long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
        while (executor.inUse() > 0 && System.nanoTime() < deadline) {
            Thread.sleep(5);
        }
        assertEquals(0, executor.inUse());
    }

    @Test
    void sanitisesItsCaps() {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(0, -5)) {
            assertEquals(VesperaStreamingExecutor.defaultMaxThreads(), executor.maxThreads());
            assertEquals(10 * Runtime.getRuntime().availableProcessors(), executor.maxThreads());
            assertEquals(0, executor.maxQueued());
        }
    }

    @Test
    void admitsThreadsPlusQueuedStreamsThenRejects() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 1)) {
            CountDownLatch release = new CountDownLatch(1);
            CountDownLatch started = new CountDownLatch(1);
            AtomicReference<Thread> worker = new AtomicReference<>();
            AtomicInteger queuedRuns = new AtomicInteger();

            VesperaStreamingExecutor.Reservation first = executor.tryReserve();
            VesperaStreamingExecutor.Reservation second = executor.tryReserve();
            assertNotNull(first);
            assertNotNull(second);
            assertNull(executor.tryReserve(), "threads + queue exhausted");
            assertEquals(2, executor.inUse());

            first.executor().execute(() -> {
                worker.set(Thread.currentThread());
                started.countDown();
                try {
                    release.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            });
            assertTrue(started.await(5, TimeUnit.SECONDS));
            assertTrue(worker.get().isDaemon());
            assertTrue(worker.get().getName().startsWith("vespera-streaming-"));
            second.executor().execute(queuedRuns::incrementAndGet);
            // Only the first task of a reservation runs.
            second.executor().execute(queuedRuns::incrementAndGet);
            // Cancelling a started reservation does not free its slot early.
            second.cancel();
            assertEquals(2, executor.inUse());
            assertNull(executor.tryReserve());

            release.countDown();
            awaitIdle(executor);
            assertEquals(1, queuedRuns.get());
            assertNotNull(executor.tryReserve(), "slots come back when the streams end");
        }
    }

    @Test
    void cancellingAnUnstartedReservationFreesTheSlotOnce() {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 0)) {
            VesperaStreamingExecutor.Reservation slot = executor.tryReserve();
            assertNotNull(slot);
            assertNull(executor.tryReserve());
            slot.cancel();
            slot.cancel();
            assertEquals(0, executor.inUse());
            AtomicInteger runs = new AtomicInteger();
            // A cancelled reservation never runs its task.
            slot.executor().execute(runs::incrementAndGet);
            assertEquals(0, runs.get());
            assertEquals(0, executor.inUse());
        }
    }

    @Test
    void closedExecutorAdmitsNothingAndReleasesRejectedSlots() throws Exception {
        VesperaStreamingExecutor executor = new VesperaStreamingExecutor(2, 2);
        VesperaStreamingExecutor.Reservation reserved = executor.tryReserve();
        assertNotNull(reserved);
        assertFalse(executor.isClosed());
        executor.close();
        assertTrue(executor.isClosed());
        assertNull(executor.tryReserve());
        assertThrows(RejectedExecutionException.class, () -> reserved.executor().execute(() -> { }));
        awaitIdle(executor);
    }
}
