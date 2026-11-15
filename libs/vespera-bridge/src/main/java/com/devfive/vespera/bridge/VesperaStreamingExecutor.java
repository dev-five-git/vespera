package com.devfive.vespera.bridge;

import java.util.ArrayDeque;
import java.util.concurrent.Executor;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.Semaphore;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.locks.Condition;
import java.util.concurrent.locks.ReentrantLock;

/**
 * Dedicated, bounded thread pool for the WebFlux proxy's blocking
 * response-streaming dispatches ({@code text/event-stream} / NDJSON).
 *
 * <p>Each streamed response holds one thread for its whole lifetime (the
 * native streaming symbol blocks until the Rust body ends), so running them on
 * Reactor's shared {@code boundedElastic} scheduler let many SSE clients starve
 * every other blocking task of the application. This pool is private to
 * vespera and capped twice:
 *
 * <ul>
 *   <li>{@code maxThreads} streams run concurrently;</li>
 *   <li>{@code maxQueued} more wait (FIFO) for a free thread;</li>
 *   <li>beyond that {@link VesperaReactiveProxyHandler} answers {@code 503}
 *       ({@link ProxyErrors#STREAMING_SATURATED}) before anything is
 *       committed.</li>
 * </ul>
 *
 * <p>A stream is handed to an idle thread whenever one is waiting; a new
 * thread starts only when none is (up to {@code maxThreads}). Idle threads
 * time out after {@value #KEEP_ALIVE_SECONDS} s; all are
 * daemon threads named {@code vespera-streaming-N}. {@link #close()} (called
 * by Spring when the context closes) stops accepting streams and lets the
 * running ones finish.
 */
public final class VesperaStreamingExecutor implements AutoCloseable {

    /** Default queue cap ({@code vespera.bridge.reactive-streaming.max-queued}). */
    public static final int DEFAULT_MAX_QUEUED = 100;

    static final long KEEP_ALIVE_SECONDS = 60;

    private final int maxThreads;
    private final int maxQueued;
    private final Workers pool;
    /** One permit per admitted stream (running or queued). */
    private final Semaphore permits;
    /** {@link #tryReserve()} calls, admitted or not (test probe; streams only). */
    private final AtomicLong reserveAttempts = new AtomicLong();

    /**
     * @param maxThreads concurrent streams; {@code <= 0} = {@link #defaultMaxThreads()}
     * @param maxQueued  streams waiting for a thread; negative = {@code 0}
     */
    public VesperaStreamingExecutor(int maxThreads, int maxQueued) {
        this(maxThreads, maxQueued, TimeUnit.SECONDS.toNanos(KEEP_ALIVE_SECONDS));
    }

    /** As {@link #VesperaStreamingExecutor(int, int)} with an explicit idle-thread keep-alive (tests). */
    VesperaStreamingExecutor(int maxThreads, int maxQueued, long keepAliveNanos) {
        this(maxThreads, maxQueued, keepAliveNanos, Thread::new);
    }

    /** As above, with the factory creating the pool threads (tests inject start failures). */
    VesperaStreamingExecutor(int maxThreads, int maxQueued, long keepAliveNanos, ThreadFactory threadFactory) {
        this.maxThreads = maxThreads > 0 ? maxThreads : defaultMaxThreads();
        this.maxQueued = Math.max(0, maxQueued);
        // Unbounded queue: admission is enforced by the permits, so a thread
        // still finishing a task can never make execute() reject a stream
        // that was already admitted.
        this.pool = new Workers(this.maxThreads, keepAliveNanos, threadFactory);
        // long math: maxThreads + maxQueued may exceed Integer.MAX_VALUE (saturate).
        this.permits = new Semaphore(capacity(this.maxThreads, this.maxQueued));
    }

    /** {@code maxThreads + maxQueued}, saturated at {@link Integer#MAX_VALUE} instead of overflowing. */
    static int capacity(int maxThreads, int maxQueued) {
        return (int) Math.min(Integer.MAX_VALUE, (long) maxThreads + (long) maxQueued);
    }

    /** {@code 10 × availableProcessors()} — the cap of Reactor's {@code boundedElastic}. */
    public static int defaultMaxThreads() {
        return 10 * Runtime.getRuntime().availableProcessors();
    }

    public int maxThreads() {
        return maxThreads;
    }

    public int maxQueued() {
        return maxQueued;
    }

    /** Streams currently admitted (running or queued). */
    public int inUse() {
        return capacity(maxThreads, maxQueued) - permits.availablePermits();
    }

    /** How many times {@link #tryReserve()} was called (admitted or refused). */
    long reserveAttempts() {
        return reserveAttempts.get();
    }

    /** Live pool threads (test probe). */
    int threadCount() {
        return pool.threadCount();
    }

    /** Pool threads waiting for a task (test probe). */
    int idleThreadCount() {
        return pool.idleCount();
    }

    /** Tasks waiting in the pool queue (test probe). */
    int queuedTaskCount() {
        return pool.queuedCount();
    }

    /** Whether {@link #close()} was called. */
    public boolean isClosed() {
        return pool.isShutdown();
    }

    /**
     * Admit one stream, or {@code null} when {@code maxThreads + maxQueued}
     * streams are already admitted (or the pool is closed).
     */
    Reservation tryReserve() {
        reserveAttempts.incrementAndGet();
        if (pool.isShutdown() || !permits.tryAcquire()) {
            return null;
        }
        return new Reservation();
    }

    /** Stops accepting streams; running and queued ones finish (threads are daemons). */
    @Override
    public void close() {
        pool.shutdown();
    }

    /**
     * One admitted stream: {@link #executor()} runs its single task on the
     * pool and frees the slot when the task ends; {@link #cancel()} frees a
     * slot whose task has not started running yet: never submitted, or
     * submitted but still waiting in the pool queue (the client went away
     * while queued). A cancelled queued task is removed from the queue and,
     * should a worker already have taken it, skipped: the native dispatch
     * never runs for a disconnected client.
     */
    final class Reservation {
        private static final int RESERVED = 0;
        private static final int QUEUED = 1;
        private static final int RUNNING = 2;
        /** Terminal: the permit was released (task ended, cancelled or rejected). */
        private static final int RELEASED = 3;

        private final AtomicInteger state = new AtomicInteger(RESERVED);
        private volatile Runnable queued;

        /** Runs the (single) task of this stream on the pool; later tasks are ignored. */
        Executor executor() {
            return task -> {
                // Claim the reservation BEFORE publishing the pooled task: an
                // ignored later task of the same reservation must not
                // overwrite `queued`, or cancel() would remove the wrong
                // task and leave the real one in the queue.
                if (!state.compareAndSet(RESERVED, QUEUED)) {
                    return;
                }
                Runnable pooled = () -> {
                    // Cancelled while it waited in the queue: never run it
                    // (the permit was already released by cancel()).
                    if (!state.compareAndSet(QUEUED, RUNNING)) {
                        return;
                    }
                    try {
                        task.run();
                    } finally {
                        release();
                    }
                };
                queued = pooled;
                try {
                    pool.execute(pooled);
                } catch (RejectedExecutionException closed) {
                    release();
                    throw closed;
                }
                // cancel() may have run between the claim and the submit (it
                // found nothing to remove yet): drop the queued task now. A
                // worker that already took it skips it (QUEUED -> RUNNING
                // fails), so the native dispatch still never runs.
                if (state.get() == RELEASED) {
                    pool.remove(pooled);
                }
            };
        }

        /**
         * Free the slot when the task has not started running (the stream was
         * cancelled first, possibly while queued); a no-op once it runs or ended.
         */
        void cancel() {
            while (true) {
                int current = state.get();
                if (current != RESERVED && current != QUEUED) {
                    return;
                }
                if (state.compareAndSet(current, RELEASED)) {
                    permits.release();
                    Runnable pooled = queued;
                    if (current == QUEUED && pooled != null) {
                        pool.remove(pooled);
                    }
                    return;
                }
            }
        }

        private void release() {
            if (state.getAndSet(RELEASED) != RELEASED) {
                permits.release();
            }
        }
    }

    /**
     * Minimal thread pool capped at {@code maxThreads}. A
     * {@code ThreadPoolExecutor} with {@code core == max} starts a new thread
     * for every task while below the core size, even when idle threads
     * wait, so work rotates through all of them and each one keeps timing
     * out and being recreated. Here a task goes to a waiting idle thread
     * first, and a thread starts only when no idle one can take it. All
     * bookkeeping runs under one lock, so a thread timing out can never
     * strand a queued task.
     */
    static final class Workers {
        private final int maxThreads;
        private final long keepAliveNanos;
        private final ThreadFactory threadFactory;
        private final AtomicInteger seq = new AtomicInteger(1);
        private final ReentrantLock lock = new ReentrantLock();
        private final Condition available = lock.newCondition();
        private final ArrayDeque<Runnable> queue = new ArrayDeque<>();
        private int threads;
        private int idle;
        private boolean shutdown;

        Workers(int maxThreads, long keepAliveNanos, ThreadFactory threadFactory) {
            this.maxThreads = maxThreads;
            this.keepAliveNanos = keepAliveNanos;
            this.threadFactory = threadFactory;
        }

        void execute(Runnable task) {
            boolean spawn;
            lock.lock();
            try {
                if (shutdown) {
                    throw new RejectedExecutionException("vespera streaming executor is closed");
                }
                queue.addLast(task);
                // An idle thread not yet claimed by an earlier queued task takes it.
                spawn = queue.size() > idle && threads < maxThreads;
                if (spawn) {
                    threads++;
                } else {
                    available.signal();
                }
            } finally {
                lock.unlock();
            }
            if (spawn) {
                startThread(task);
            }
        }

        /**
         * Start a thread counted (by the caller, under the lock) for
         * {@code task}. On failure the count is undone and the task leaves the
         * queue, unless a running worker that finished its own task already
         * took it (it was queued before the start): it is being served, so
         * the reservation must keep its slot and nothing is rejected.
         */
        private void startThread(Runnable task) {
            Throwable failed = tryStart();
            if (failed == null) {
                return;
            }
            boolean stillQueued;
            lock.lock();
            try {
                stillQueued = queue.remove(task);
            } finally {
                lock.unlock();
            }
            if (stillQueued) {
                throw new RejectedExecutionException("cannot start a streaming thread", failed);
            }
        }

        /**
         * Start one counted pool thread; on failure uncount it and return the
         * failure ({@code null} once started).
         */
        private Throwable tryStart() {
            try {
                Thread thread = threadFactory.newThread(this::work);
                thread.setName("vespera-streaming-" + seq.getAndIncrement());
                thread.setDaemon(true);
                thread.start();
                return null;
            } catch (OutOfMemoryError | RuntimeException failed) {
                lock.lock();
                try {
                    threads--;
                } finally {
                    lock.unlock();
                }
                return failed;
            }
        }

        void remove(Runnable task) {
            lock.lock();
            try {
                queue.remove(task);
            } finally {
                lock.unlock();
            }
        }

        /** Stop accepting tasks; queued ones still run, then the threads exit. */
        void shutdown() {
            lock.lock();
            try {
                shutdown = true;
                available.signalAll();
            } finally {
                lock.unlock();
            }
        }

        boolean isShutdown() {
            lock.lock();
            try {
                return shutdown;
            } finally {
                lock.unlock();
            }
        }

        int threadCount() {
            lock.lock();
            try {
                return threads;
            } finally {
                lock.unlock();
            }
        }

        int idleCount() {
            lock.lock();
            try {
                return idle;
            } finally {
                lock.unlock();
            }
        }

        int queuedCount() {
            lock.lock();
            try {
                return queue.size();
            } finally {
                lock.unlock();
            }
        }

        private void work() {
            boolean exitedNormally = false;
            try {
                Runnable task;
                while ((task = next()) != null) {
                    try {
                        task.run();
                    } catch (RuntimeException | Error failed) {
                        report(failed);
                    }
                }
                exitedNormally = true;
            } finally {
                if (!exitedNormally) {
                    workerDied();
                }
            }
        }

        /**
         * Same as a ThreadPoolExecutor worker, but the thread survives: report
         * the failure and take the next task. A throwing handler (an
         * application default handler that rethrows, or one failing under
         * OOM) must not end the thread either: it would stay counted in
         * {@code threads}, and after {@code maxThreads} such deaths no thread
         * would ever start again to drain the queue.
         */
        private static void report(Throwable failed) {
            Thread current = Thread.currentThread();
            try {
                current.getUncaughtExceptionHandler().uncaughtException(current, failed);
            } catch (RuntimeException | Error ignored) {
                // Nowhere left to report it.
            }
        }

        /**
         * A worker left its loop abnormally ({@link #next()} threw): uncount
         * it and, when queued tasks have no idle thread to take them, start a
         * replacement so they are not stranded.
         */
        private void workerDied() {
            boolean spawn;
            lock.lock();
            try {
                threads--;
                spawn = queue.size() > idle && threads < maxThreads;
                if (spawn) {
                    threads++;
                }
            } finally {
                lock.unlock();
            }
            if (spawn) {
                tryStart();
            }
        }

        /** The next queued task, or {@code null} when this thread should exit. */
        private Runnable next() {
            lock.lock();
            try {
                long remaining = keepAliveNanos;
                while (true) {
                    Runnable task = queue.pollFirst();
                    if (task != null) {
                        return task;
                    }
                    if (shutdown || remaining <= 0) {
                        threads--;
                        return null;
                    }
                    idle++;
                    try {
                        remaining = available.awaitNanos(remaining);
                    } catch (InterruptedException interrupted) {
                        // vespera never interrupts its pool threads: treat
                        // it as the keep-alive expiring.
                        remaining = 0;
                    } finally {
                        idle--;
                    }
                }
            } finally {
                lock.unlock();
            }
        }
    }
}
