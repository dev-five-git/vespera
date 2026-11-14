package com.devfive.vespera.bridge;

import java.util.concurrent.Executor;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.RejectedExecutionException;
import java.util.concurrent.Semaphore;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

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
 * <p>Idle threads time out after {@value #KEEP_ALIVE_SECONDS} s; all are
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
    private final ThreadPoolExecutor pool;
    /** One permit per admitted stream (running or queued). */
    private final Semaphore permits;

    /**
     * @param maxThreads concurrent streams; {@code <= 0} = {@link #defaultMaxThreads()}
     * @param maxQueued  streams waiting for a thread; negative = {@code 0}
     */
    public VesperaStreamingExecutor(int maxThreads, int maxQueued) {
        this.maxThreads = maxThreads > 0 ? maxThreads : defaultMaxThreads();
        this.maxQueued = Math.max(0, maxQueued);
        AtomicInteger seq = new AtomicInteger(1);
        // Unbounded queue: admission is enforced by the permits, so a thread
        // still finishing a task can never make execute() reject a stream
        // that was already admitted.
        this.pool = new ThreadPoolExecutor(this.maxThreads, this.maxThreads,
                KEEP_ALIVE_SECONDS, TimeUnit.SECONDS, new LinkedBlockingQueue<>(), task -> {
                    Thread thread = new Thread(task, "vespera-streaming-" + seq.getAndIncrement());
                    thread.setDaemon(true);
                    return thread;
                });
        this.pool.allowCoreThreadTimeOut(true);
        this.permits = new Semaphore(this.maxThreads + this.maxQueued);
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
        return maxThreads + maxQueued - permits.availablePermits();
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
     * slot whose task never started.
     */
    final class Reservation {
        private static final int RESERVED = 0;
        private static final int STARTED = 1;
        private static final int RELEASED = 2;

        private final AtomicInteger state = new AtomicInteger(RESERVED);

        /** Runs the (single) task of this stream on the pool; later tasks are ignored. */
        Executor executor() {
            return task -> {
                if (!state.compareAndSet(RESERVED, STARTED)) {
                    return;
                }
                try {
                    pool.execute(() -> {
                        try {
                            task.run();
                        } finally {
                            release();
                        }
                    });
                } catch (RejectedExecutionException closed) {
                    release();
                    throw closed;
                }
            };
        }

        /** Free the slot when the task never started (the stream was cancelled first). */
        void cancel() {
            if (state.compareAndSet(RESERVED, RELEASED)) {
                permits.release();
            }
        }

        private void release() {
            if (state.getAndSet(RELEASED) != RELEASED) {
                permits.release();
            }
        }
    }
}
