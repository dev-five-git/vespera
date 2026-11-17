package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.time.Duration;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executor;
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

    /** {@code max-threads + max-queued} beyond {@code int} saturates instead of going negative. */
    @Test
    void hugeCapsSaturateInsteadOfOverflowing() {
        assertEquals(Integer.MAX_VALUE, VesperaStreamingExecutor.capacity(Integer.MAX_VALUE, 100));
        assertEquals(Integer.MAX_VALUE, VesperaStreamingExecutor.capacity(Integer.MAX_VALUE, Integer.MAX_VALUE));
        assertEquals(5, VesperaStreamingExecutor.capacity(2, 3));
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(Integer.MAX_VALUE, 100)) {
            assertEquals(0, executor.inUse());
            VesperaStreamingExecutor.Reservation slot = executor.tryReserve();
            assertNotNull(slot, "admits a stream");
            assertEquals(1, executor.inUse());
            slot.cancel();
            assertEquals(0, executor.inUse());
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
            // Cancelling a running reservation does not free its slot early.
            first.cancel();
            assertEquals(2, executor.inUse());
            assertNull(executor.tryReserve());

            release.countDown();
            awaitIdle(executor);
            assertEquals(1, queuedRuns.get());
            assertNotNull(executor.tryReserve(), "slots come back when the streams end");
        }
    }

    @Test
    void cancellingAQueuedReservationDropsItsTaskAndFreesTheSlot() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 1)) {
            CountDownLatch release = new CountDownLatch(1);
            CountDownLatch started = new CountDownLatch(1);
            AtomicInteger queuedRuns = new AtomicInteger();
            VesperaStreamingExecutor.Reservation running = executor.tryReserve();
            VesperaStreamingExecutor.Reservation queued = executor.tryReserve();
            running.executor().execute(() -> {
                started.countDown();
                try {
                    release.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            });
            assertTrue(started.await(5, TimeUnit.SECONDS));
            // Submitted, but waiting for the only thread.
            queued.executor().execute(queuedRuns::incrementAndGet);
            assertEquals(2, executor.inUse());

            // The client went away while queued: the slot comes back at once.
            queued.cancel();
            queued.cancel();
            assertEquals(1, executor.inUse());
            VesperaStreamingExecutor.Reservation next = executor.tryReserve();
            assertNotNull(next, "the queued slot is free again");
            next.cancel();

            release.countDown();
            awaitIdle(executor);
            Thread.sleep(50);
            assertEquals(0, queuedRuns.get(), "a cancelled queued task never runs");
            assertEquals(0, executor.inUse());
        }
    }

    @Test
    void cancellingAnUnstartedReservationFreesTheSlotOnce() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 0)) {
            VesperaStreamingExecutor.Reservation slot = executor.tryReserve();
            assertNotNull(slot);
            assertNull(executor.tryReserve());
            slot.cancel();
            slot.cancel();
            assertEquals(0, executor.inUse());
            CountDownLatch ran = new CountDownLatch(1);
            // A cancelled reservation never runs its task (on any thread):
            // give a wrongly submitted task time to run before asserting.
            slot.executor().execute(ran::countDown);
            assertFalse(ran.await(300, TimeUnit.MILLISECONDS), "a cancelled reservation ran its task");
            assertEquals(0, executor.inUse());
        }
    }

    private static void awaitCondition(java.util.function.BooleanSupplier condition, String what)
            throws InterruptedException {
        long deadline = System.nanoTime() + Duration.ofSeconds(5).toNanos();
        while (!condition.getAsBoolean() && System.nanoTime() < deadline) {
            Thread.sleep(2);
        }
        assertTrue(condition.getAsBoolean(), what);
    }

    /**
     * Sequential streams reuse the one idle thread. A {@code ThreadPoolExecutor}
     * with {@code core == max} started a new thread per task until the core
     * size was reached (and then rotated work through all of them, so every
     * thread kept timing out and being recreated).
     */
    @Test
    void sequentialStreamsReuseTheIdleThread() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(4, 0)) {
            java.util.Set<Thread> used = java.util.concurrent.ConcurrentHashMap.newKeySet();
            for (int i = 0; i < 8; i++) {
                CountDownLatch ran = new CountDownLatch(1);
                VesperaStreamingExecutor.Reservation slot = executor.tryReserve();
                assertNotNull(slot);
                slot.executor().execute(() -> {
                    used.add(Thread.currentThread());
                    ran.countDown();
                });
                assertTrue(ran.await(5, TimeUnit.SECONDS));
                awaitCondition(() -> executor.idleThreadCount() == 1, "the thread went idle");
            }
            assertEquals(1, used.size(), "every stream ran on the same idle thread");
            assertEquals(1, executor.threadCount());
        }
    }

    /** Concurrent streams start new threads up to {@code maxThreads}, then queue. */
    @Test
    void concurrentStreamsStartThreadsUpToTheCapThenQueue() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(2, 2)) {
            CountDownLatch release = new CountDownLatch(1);
            CountDownLatch started = new CountDownLatch(2);
            AtomicInteger runs = new AtomicInteger();
            Runnable blocking = () -> {
                started.countDown();
                try {
                    release.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
                runs.incrementAndGet();
            };
            executor.tryReserve().executor().execute(blocking);
            executor.tryReserve().executor().execute(blocking);
            assertTrue(started.await(5, TimeUnit.SECONDS));
            executor.tryReserve().executor().execute(runs::incrementAndGet);
            executor.tryReserve().executor().execute(runs::incrementAndGet);
            assertEquals(2, executor.threadCount(), "capped at maxThreads");
            assertEquals(2, executor.queuedTaskCount());
            release.countDown();
            awaitIdle(executor);
            assertEquals(4, runs.get());
        }
    }

    /** An idle thread past its keep-alive exits; the next stream starts a new one. */
    @Test
    void idleThreadsTimeOutAndAreRecreatedOnDemand() throws Exception {
        try (VesperaStreamingExecutor executor =
                     new VesperaStreamingExecutor(2, 0, TimeUnit.MILLISECONDS.toNanos(20))) {
            CountDownLatch first = new CountDownLatch(1);
            executor.tryReserve().executor().execute(first::countDown);
            assertTrue(first.await(5, TimeUnit.SECONDS));
            awaitCondition(() -> executor.threadCount() == 0, "the idle thread timed out");
            CountDownLatch second = new CountDownLatch(1);
            executor.tryReserve().executor().execute(second::countDown);
            assertTrue(second.await(5, TimeUnit.SECONDS));
            awaitIdle(executor);
        }
    }

    /** A throwing stream task frees its slot and its thread keeps serving. */
    @Test
    void aThrowingTaskFreesItsSlotAndKeepsTheThread() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 0)) {
            AtomicReference<Thread> failedOn = new AtomicReference<>();
            CountDownLatch reported = new CountDownLatch(1);
            CountDownLatch thrown = new CountDownLatch(1);
            executor.tryReserve().executor().execute(() -> {
                Thread.currentThread().setUncaughtExceptionHandler((t, e) -> reported.countDown());
                failedOn.set(Thread.currentThread());
                thrown.countDown();
                throw new IllegalStateException("boom");
            });
            assertTrue(thrown.await(5, TimeUnit.SECONDS));
            assertTrue(reported.await(5, TimeUnit.SECONDS), "reported to the uncaught handler");
            awaitIdle(executor);
            AtomicReference<Thread> nextOn = new AtomicReference<>();
            CountDownLatch ran = new CountDownLatch(1);
            executor.tryReserve().executor().execute(() -> {
                nextOn.set(Thread.currentThread());
                ran.countDown();
            });
            assertTrue(ran.await(5, TimeUnit.SECONDS));
            assertEquals(failedOn.get(), nextOn.get());
        }
    }

    /**
     * A stream task throws and so does the thread's uncaught-exception
     * handler (an application default handler that rethrows): the worker
     * used to die still counted in {@code threads}, so with
     * {@code maxThreads = 1} no thread ever started again and the next
     * admitted stream never ran.
     */
    @Test
    void aThrowingUncaughtHandlerDoesNotStrandLaterStreams() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 0)) {
            CountDownLatch thrown = new CountDownLatch(1);
            executor.tryReserve().executor().execute(() -> {
                Thread.currentThread().setUncaughtExceptionHandler((t, e) -> {
                    throw new IllegalStateException("the handler fails too");
                });
                thrown.countDown();
                throw new IllegalStateException("boom");
            });
            assertTrue(thrown.await(5, TimeUnit.SECONDS));
            awaitIdle(executor);
            CountDownLatch ran = new CountDownLatch(1);
            executor.tryReserve().executor().execute(ran::countDown);
            assertTrue(ran.await(5, TimeUnit.SECONDS), "the next stream still runs");
            awaitIdle(executor);
            assertEquals(1, executor.threadCount());
        }
    }

    /**
     * {@code Thread.start()} fails for a new thread after a running worker
     * already took the queued task (it finished its own stream first): the
     * task is being served, so {@code execute} must neither reject it nor
     * free its slot while it runs (that admitted one stream too many and
     * answered the client a proxy error while Rust still streamed).
     */
    @Test
    void aFailedThreadStartKeepsTheSlotOfATaskAnotherWorkerTook() throws Exception {
        CountDownLatch releaseFirst = new CountDownLatch(1);
        CountDownLatch secondStarted = new CountDownLatch(1);
        CountDownLatch releaseSecond = new CountDownLatch(1);
        AtomicInteger created = new AtomicInteger();
        java.util.concurrent.ThreadFactory factory = runnable -> {
            if (created.incrementAndGet() == 1) {
                return new Thread(runnable);
            }
            return new Thread(runnable) {
                @Override
                public synchronized void start() {
                    // The running worker ends its stream and takes the queued one.
                    releaseFirst.countDown();
                    try {
                        assertTrue(secondStarted.await(5, TimeUnit.SECONDS));
                    } catch (InterruptedException e) {
                        Thread.currentThread().interrupt();
                    }
                    throw new OutOfMemoryError("unable to create native thread");
                }
            };
        };
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(
                2, 1, TimeUnit.SECONDS.toNanos(60), factory)) {
            CountDownLatch firstStarted = new CountDownLatch(1);
            executor.tryReserve().executor().execute(() -> {
                firstStarted.countDown();
                try {
                    releaseFirst.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            });
            assertTrue(firstStarted.await(5, TimeUnit.SECONDS));
            VesperaStreamingExecutor.Reservation second = executor.tryReserve();
            second.executor().execute(() -> {
                secondStarted.countDown();
                try {
                    releaseSecond.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            });
            awaitCondition(() -> executor.inUse() == 1, "the first stream ended");
            assertEquals(1, executor.inUse(), "the running second stream keeps its slot");
            assertEquals(1, executor.threadCount());
            releaseSecond.countDown();
            awaitIdle(executor);
        }
    }

    /**
     * A thread start fails while another worker is still busy with a stream:
     * that worker takes the queued task when its stream ends, so the task
     * keeps its slot and runs. Rejecting it at once answered 503 although the
     * queue had room and a live thread would free up.
     */
    @Test
    void aFailedThreadStartKeepsQueuedTasksWhileAnotherWorkerIsBusy() throws Exception {
        CountDownLatch releaseFirst = new CountDownLatch(1);
        AtomicInteger created = new AtomicInteger();
        java.util.concurrent.ThreadFactory factory = runnable -> {
            if (created.incrementAndGet() == 1) {
                return new Thread(runnable);
            }
            return new Thread(runnable) {
                @Override
                public synchronized void start() {
                    throw new OutOfMemoryError("unable to create native thread");
                }
            };
        };
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(
                2, 1, TimeUnit.SECONDS.toNanos(60), factory)) {
            CountDownLatch firstStarted = new CountDownLatch(1);
            executor.tryReserve().executor().execute(() -> {
                firstStarted.countDown();
                try {
                    releaseFirst.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            });
            assertTrue(firstStarted.await(5, TimeUnit.SECONDS));
            CountDownLatch secondRan = new CountDownLatch(1);
            VesperaStreamingExecutor.Reservation second = executor.tryReserve();
            second.executor().execute(secondRan::countDown);
            assertFalse(second.wasRejected(), "queued behind the busy worker, not rejected");
            assertEquals(1, executor.queuedTaskCount());

            releaseFirst.countDown();
            assertTrue(secondRan.await(5, TimeUnit.SECONDS), "the busy worker takes the queued task");
            assertFalse(second.wasRejected());
            awaitCondition(() -> executor.inUse() == 0, "both streams ended");
        }
    }

    /**
     * A task queued behind a thread start that then fails had no thread
     * left (round-14 L-1): it waited, holding its slot, until some later
     * stream happened to start a thread. Once no pool thread is left, every
     * queued task is rejected and frees its slot.
     */
    @Test
    void aFailedThreadStartRejectsEveryTaskQueuedBehindIt() throws Exception {
        CountDownLatch secondQueued = new CountDownLatch(1);
        AtomicInteger created = new AtomicInteger();
        java.util.concurrent.ThreadFactory factory = runnable -> {
            if (created.incrementAndGet() > 1) {
                return new Thread(runnable);
            }
            return new Thread(runnable) {
                @Override
                public synchronized void start() {
                    try {
                        assertTrue(secondQueued.await(5, TimeUnit.SECONDS));
                    } catch (InterruptedException e) {
                        Thread.currentThread().interrupt();
                    }
                    throw new OutOfMemoryError("unable to create native thread");
                }
            };
        };
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(
                1, 1, TimeUnit.SECONDS.toNanos(60), factory)) {
            AtomicInteger ran = new AtomicInteger();
            VesperaStreamingExecutor.Reservation first = executor.tryReserve();
            Thread submitter = new Thread(() -> first.executor().execute(ran::incrementAndGet));
            submitter.start();
            awaitCondition(() -> executor.queuedTaskCount() == 1, "the first task queued");
            VesperaStreamingExecutor.Reservation second = executor.tryReserve();
            second.executor().execute(ran::incrementAndGet);
            assertEquals(2, executor.queuedTaskCount(), "queued behind the in-flight start");
            secondQueued.countDown();
            submitter.join(5000);
            assertFalse(submitter.isAlive());

            RejectedExecutionException firstFailure =
                    first.rejection().toCompletableFuture().get(5, TimeUnit.SECONDS);
            RejectedExecutionException secondFailure =
                    second.rejection().toCompletableFuture().get(5, TimeUnit.SECONDS);
            assertNotNull(firstFailure);
            assertNotNull(secondFailure, "the second task is rejected, not stranded");
            assertTrue(secondFailure.getCause() instanceof OutOfMemoryError);
            assertTrue(first.wasRejected() && second.wasRejected());
            assertEquals(0, executor.inUse(), "both slots are free");
            assertEquals(0, executor.queuedTaskCount());
            assertEquals(0, executor.threadCount());
            second.cancel(); // no-op after the rejection
            assertEquals(0, executor.inUse());

            // The pool recovers: the next stream starts a thread and runs.
            VesperaStreamingExecutor.Reservation third = executor.tryReserve();
            third.executor().execute(ran::incrementAndGet);
            awaitCondition(() -> ran.get() == 1, "a later stream runs");
            assertNull(third.rejection().toCompletableFuture().get(5, TimeUnit.SECONDS));
            assertFalse(third.wasRejected());
            assertEquals(1, ran.get(), "the rejected tasks never ran");
            awaitIdle(executor);
        }
    }

    /**
     * An ignored second task of a reservation must not replace the queued
     * one: {@code cancel()} then removed the never-submitted second task and
     * left the real one in the queue.
     */
    @Test
    void cancelRemovesTheQueuedTaskEvenAfterAnIgnoredSecondTask() throws Exception {
        try (VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 1)) {
            CountDownLatch release = new CountDownLatch(1);
            CountDownLatch started = new CountDownLatch(1);
            executor.tryReserve().executor().execute(() -> {
                started.countDown();
                try {
                    release.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            });
            assertTrue(started.await(5, TimeUnit.SECONDS));
            VesperaStreamingExecutor.Reservation queued = executor.tryReserve();
            Executor queuedExecutor = queued.executor();
            queuedExecutor.execute(() -> { });
            queuedExecutor.execute(() -> { });
            assertEquals(1, executor.queuedTaskCount());
            queued.cancel();
            assertEquals(0, executor.queuedTaskCount(), "the cancelled task left the queue");
            release.countDown();
            awaitIdle(executor);
        }
    }

    /** {@link VesperaStreamingExecutor#close()} lets queued streams still run. */
    @Test
    void closeLetsQueuedStreamsFinish() throws Exception {
        VesperaStreamingExecutor executor = new VesperaStreamingExecutor(1, 1);
        CountDownLatch release = new CountDownLatch(1);
        CountDownLatch started = new CountDownLatch(1);
        CountDownLatch queuedRan = new CountDownLatch(1);
        executor.tryReserve().executor().execute(() -> {
            started.countDown();
            try {
                release.await();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        });
        assertTrue(started.await(5, TimeUnit.SECONDS));
        executor.tryReserve().executor().execute(queuedRan::countDown);
        executor.close();
        release.countDown();
        assertTrue(queuedRan.await(5, TimeUnit.SECONDS));
        awaitCondition(() -> executor.threadCount() == 0, "threads exit after close");
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
        assertTrue(reserved.wasRejected());
        assertNull(reserved.rejection().toCompletableFuture().getNow(new RejectedExecutionException()),
                "a synchronous refusal reports through the thrown exception only");
        awaitIdle(executor);
    }
}
