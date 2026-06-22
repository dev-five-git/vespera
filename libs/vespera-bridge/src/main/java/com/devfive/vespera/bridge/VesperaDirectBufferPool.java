package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.util.Map;
import java.util.Objects;

import com.devfive.vespera.bridge.VesperaBridge.BufferTooSmallException;
import com.devfive.vespera.bridge.VesperaBridge.HeaderSource;
import com.devfive.vespera.bridge.VesperaWireCodec.ExposedByteArrayOutputStream;

/**
 * Per-thread reusable <strong>direct</strong> {@link ByteBuffer} pool
 * for the {@link VesperaBridge#dispatchDirect(ByteBuffer, int, ByteBuffer)}
 * fast path — the allocation-amortising layer that backs the public
 * {@code dispatchDirectPooled} entry points.
 *
 * <p>Split out of {@link VesperaBridge} (which owns only JNI-symbol-bound
 * native methods + library loading): this class holds the off-heap buffer
 * pooling, adaptive retention, virtual-thread fallback, and overflow-retry
 * policy.  It calls {@link VesperaBridge#dispatchDirect} /
 * {@link VesperaBridge#dispatchBytes} for the actual native dispatch and
 * {@link VesperaWireCodec} for wire encoding.
 *
 * <p><strong>Virtual thread (Project Loom) limitation:</strong> the pool
 * is backed by {@link ThreadLocal}, which binds to the <em>virtual</em>
 * thread (not the carrier) in Java 21+.  {@link #currentThreadIsVirtual()}
 * detects this and routes virtual threads to the GC-managed heap
 * {@link VesperaBridge#dispatchBytes(byte[])} path so off-heap memory does
 * not accumulate per vthread.
 */
final class VesperaDirectBufferPool {

    private VesperaDirectBufferPool() {}

    /** Initial per-thread direct buffer capacity (64 KiB). */
    private static final int DIRECT_INITIAL_CAPACITY = 64 * 1024;

    /**
     * Maximum per-thread direct buffer capacity (default 4 MiB,
     * overridable via the {@code vespera.direct.maxBufferBytes} system
     * property, clamped to 64 KiB–256 MiB). Payloads beyond the cap fall
     * back to {@link VesperaBridge#dispatchBytes(byte[])}.
     */
    private static final int DIRECT_MAX_HARD_CAPACITY = 256 * 1024 * 1024;
    private static final int DIRECT_MAX_CAPACITY = directMaxCapacity();

    private static int directMaxCapacity() {
        int configured = Integer.getInteger("vespera.direct.maxBufferBytes", 4 * 1024 * 1024);
        return Math.max(DIRECT_INITIAL_CAPACITY, Math.min(DIRECT_MAX_HARD_CAPACITY, configured));
    }

    /**
     * Per-thread <strong>hard retention cap</strong> for the pooled
     * direct buffers (system property
     * {@code vespera.direct.maxRetainedBytes}, default 2 MiB; clamped
     * to [{@link #DIRECT_INITIAL_CAPACITY}, {@link #DIRECT_MAX_CAPACITY}]).
     *
     * <p>A buffer that a large dispatch grew beyond this cap is shrunk
     * back to {@link #DIRECT_INITIAL_CAPACITY} <strong>adaptively</strong>
     * — only after {@link #DIRECT_SHRINK_IDLE_DISPATCHES} consecutive
     * dispatches stayed under the cap (so a repeatedly-large idempotent
     * endpoint keeps its buffer instead of shrink/overflow/re-run on
     * every call), yet a thread that stops handling large responses
     * still releases the off-heap memory.  Transient growth up to
     * {@link #DIRECT_MAX_CAPACITY} for an individual request is always
     * allowed — only steady-state retention is capped.
     */
    private static final int DIRECT_RETAIN_CAPACITY = Math.max(
            DIRECT_INITIAL_CAPACITY,
            Math.min(DIRECT_MAX_CAPACITY,
                    Integer.getInteger("vespera.direct.maxRetainedBytes", 2 * 1024 * 1024)));

    /**
     * Index 0 = request buffer, index 1 = response buffer.
     *
     * <p>Held strongly per platform thread so baseline direct buffers stay
     * resident on the hot DIRECT path. Oversized buffers are shrunk
     * deterministically by {@link #recordDirectPoolUse(ByteBuffer[], int, int)}
     * after an idle streak instead of relying on heap-pressure-driven soft
     * reference clearing to manage off-heap memory.
     */
    private static final ThreadLocal<ByteBuffer[]> DIRECT_POOL = new ThreadLocal<>();

    private static final int DIRECT_SHRINK_IDLE_DISPATCHES = 8;
    private static final ThreadLocal<Integer> DIRECT_UNDER_RETAIN_STREAK =
            ThreadLocal.withInitial(() -> 0);

    /**
     * Handle to {@code Thread.isVirtual()} (final API since Java 21),
     * resolved reflectively so this library still compiles and runs on
     * the Java 17 baseline.  {@code null} on pre-21 runtimes, where no
     * thread is ever virtual.
     */
    private static final java.lang.invoke.MethodHandle IS_VIRTUAL = resolveIsVirtual();

    private static java.lang.invoke.MethodHandle resolveIsVirtual() {
        try {
            return java.lang.invoke.MethodHandles.lookup()
                    .findVirtual(Thread.class, "isVirtual",
                            java.lang.invoke.MethodType.methodType(boolean.class));
        } catch (ReflectiveOperationException pre21Runtime) {
            return null;
        }
    }

    /**
     * Whether the calling thread is a virtual thread (Java 21+); always
     * {@code false} on the Java 17 baseline runtime.
     *
     * <p>The pooled direct-buffer fast path is backed by
     * {@link ThreadLocal}, which binds to the <em>virtual</em> thread
     * (not its carrier) in Java 21+ — so on a virtual-thread-per-request
     * server every dispatch would allocate a fresh direct buffer and
     * accumulate off-heap memory until GC.  {@link #dispatchDirectPooled}
     * detects this and routes virtual threads to the GC-managed heap
     * {@link VesperaBridge#dispatchBytes(byte[])} path instead.
     */
    static boolean currentThreadIsVirtual() {
        if (IS_VIRTUAL == null) {
            return false;
        }
        try {
            return (boolean) IS_VIRTUAL.invokeExact(Thread.currentThread());
        } catch (RuntimeException | Error fatalMustPropagate) {
            // JVM Errors (OutOfMemoryError, StackOverflowError, …) and runtime
            // exceptions are never the reflective-fallback case — let them
            // propagate instead of silently reporting "not virtual".
            throw fatalMustPropagate;
        } catch (Throwable reflectiveFailureFallBackToPooled) {
            // MethodHandle.invokeExact is declared `throws Throwable`; the only
            // residual checked failure here is a reflective/linkage problem
            // resolving Thread.isVirtual() — fall back to the non-virtual
            // (pooled) path, preserving the prior behavior.
            return false;
        }
    }

    /**
     * Resolve the calling thread's pooled direct buffers, (re)allocating
     * a baseline pair when none exists for this thread.
     */
    private static ByteBuffer[] directPool() {
        ByteBuffer[] pool = DIRECT_POOL.get();
        if (pool == null) {
            pool = new ByteBuffer[] {
                    ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY),
                    ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY)};
            DIRECT_POOL.set(pool);
            DIRECT_UNDER_RETAIN_STREAK.set(0);
            return pool;
        }
        return pool;
    }

    private static void recordDirectPoolUse(ByteBuffer[] pool, int requestLen, int responseLen) {
        if (requestLen > DIRECT_RETAIN_CAPACITY || responseLen > DIRECT_RETAIN_CAPACITY) {
            DIRECT_UNDER_RETAIN_STREAK.set(0);
            return;
        }
        int streak = DIRECT_UNDER_RETAIN_STREAK.get() + 1;
        if (streak < DIRECT_SHRINK_IDLE_DISPATCHES) {
            DIRECT_UNDER_RETAIN_STREAK.set(streak);
            return;
        }
        boolean requestGrown = pool[0].capacity() > DIRECT_RETAIN_CAPACITY;
        boolean responseGrown = pool[1].capacity() > DIRECT_RETAIN_CAPACITY;
        if (requestGrown) {
            pool[0] = ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY);
        }
        if (responseGrown) {
            pool[1] = ByteBuffer.allocateDirect(DIRECT_INITIAL_CAPACITY);
        }
        DIRECT_UNDER_RETAIN_STREAK.set(0);
    }

    static void clearCurrentThreadBuffers() {
        DIRECT_POOL.remove();
        DIRECT_UNDER_RETAIN_STREAK.remove();
    }

    static boolean directPoolPresentForTest() {
        return DIRECT_POOL.get() != null;
    }

    static ByteBuffer[] directPoolForTest() {
        return directPool();
    }

    static void recordDirectPoolUseForTest(ByteBuffer[] pool, int requestLen, int responseLen) {
        recordDirectPoolUse(pool, requestLen, responseLen);
    }

    /** Smallest power-of-two-ish growth ≥ {@code needed}, capped. */
    private static int grownCapacity(int needed) {
        int cap = DIRECT_INITIAL_CAPACITY;
        while (cap < needed) {
            cap = Math.min(cap * 2, DIRECT_MAX_CAPACITY);
            if (cap == DIRECT_MAX_CAPACITY) break;
        }
        return Math.max(cap, needed);
    }

    /**
     * Pooled convenience around {@link VesperaBridge#dispatchDirect(ByteBuffer,
     * int, ByteBuffer)} using per-thread reusable direct buffers (64 KiB
     * initial, doubling up to {@code vespera.direct.maxBufferBytes},
     * default 4 MiB).  See {@link VesperaBridge#dispatchDirectPooled(byte[],
     * boolean)} for the full contract.
     */
    static ByteBuffer dispatchDirectPooled(byte[] wireRequest, boolean retryOnOverflow) {
        return dispatchDirectPooled(wireRequest, retryOnOverflow, currentThreadIsVirtual());
    }

    static ByteBuffer dispatchDirectPooled(
            byte[] wireRequest, boolean retryOnOverflow, boolean currentThreadIsVirtual) {
        Objects.requireNonNull(wireRequest, "wireRequest");
        if (currentThreadIsVirtual || wireRequest.length > DIRECT_MAX_CAPACITY) {
            // Virtual thread: the per-thread direct buffer pool would
            // accumulate off-heap memory per vthread (ThreadLocal binds to
            // the vthread, not the carrier) — use the GC-managed heap path.
            // Oversized request (> cap): byte[] fallback is safe for any
            // method because no dispatch has run yet.
            return ByteBuffer.wrap(VesperaBridge.dispatchBytes(wireRequest)).asReadOnlyBuffer();
        }
        ByteBuffer[] pool = directPool();
        if (pool[0].capacity() < wireRequest.length) {
            pool[0] = ByteBuffer.allocateDirect(grownCapacity(wireRequest.length));
        }
        ByteBuffer in = pool[0];
        in.clear();
        in.put(wireRequest);

        return dispatchViaPool(pool, wireRequest.length, retryOnOverflow);
    }

    static ByteBuffer dispatchDirectPooled(
            String appName,
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body,
            boolean retryOnOverflow) {
        byte[] bodyBytes = body != null ? body : VesperaWireCodec.EMPTY_BODY;
        ExposedByteArrayOutputStream hdr =
                VesperaWireCodec.fillHeaderJson(appName, method, path, query, headers);
        try {
            int headerLen = hdr.size();
            int total = VesperaWireCodec.wireTotalLength(headerLen, bodyBytes.length);
            if (currentThreadIsVirtual() || total > DIRECT_MAX_CAPACITY) {
                // Virtual thread: avoid the per-vthread off-heap direct buffer
                // accumulation — use the GC-managed heap path.  Oversized
                // request (> cap): byte[] fallback is safe for any method
                // because no dispatch has run yet.  The reusable header buffer
                // is consumed here, before any other fillHeaderJson call.
                byte[] wire = VesperaWireCodec.assembleWire(hdr.backingArray(), headerLen, bodyBytes);
                return ByteBuffer.wrap(VesperaBridge.dispatchBytes(wire)).asReadOnlyBuffer();
            }
            ByteBuffer[] pool = directPool();
            if (pool[0].capacity() < total) {
                pool[0] = ByteBuffer.allocateDirect(grownCapacity(total));
            }
            // Consume the reusable header buffer into the pooled direct buffer.
            int written = VesperaWireCodec.assembleInto(hdr.backingArray(), headerLen, bodyBytes, pool[0]);
            if (written != total) {
                throw new IllegalStateException(
                        "assembleInto wrote " + written + ", expected " + total);
            }
            return dispatchViaPool(pool, total, retryOnOverflow);
        } finally {
            VesperaWireCodec.shrinkHeaderBufferIfOversized(hdr);
        }
    }

    static ByteBuffer dispatchDirectPooled(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body,
            boolean retryOnOverflow) {
        return dispatchDirectPooled(
                appName, method, path, query, headers, body,
                retryOnOverflow, currentThreadIsVirtual());
    }

    static ByteBuffer dispatchDirectPooled(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body,
            boolean retryOnOverflow,
            boolean currentThreadIsVirtual) {
        byte[] bodyBytes = body != null ? body : VesperaWireCodec.EMPTY_BODY;
        ExposedByteArrayOutputStream hdr =
                VesperaWireCodec.fillHeaderJson(appName, method, path, query, headers);
        try {
            int headerLen = hdr.size();
            int total = VesperaWireCodec.wireTotalLength(headerLen, bodyBytes.length);
            if (currentThreadIsVirtual || total > DIRECT_MAX_CAPACITY) {
                byte[] wire = VesperaWireCodec.assembleWire(hdr.backingArray(), headerLen, bodyBytes);
                return ByteBuffer.wrap(VesperaBridge.dispatchBytes(wire)).asReadOnlyBuffer();
            }
            ByteBuffer[] pool = directPool();
            if (pool[0].capacity() < total) {
                pool[0] = ByteBuffer.allocateDirect(grownCapacity(total));
            }
            int written = VesperaWireCodec.assembleInto(hdr.backingArray(), headerLen, bodyBytes, pool[0]);
            if (written != total) {
                throw new IllegalStateException(
                        "assembleInto wrote " + written + ", expected " + total);
            }
            return dispatchViaPool(pool, total, retryOnOverflow);
        } finally {
            VesperaWireCodec.shrinkHeaderBufferIfOversized(hdr);
        }
    }

    /**
     * Dispatch the request already prepared in the pooled in-buffer
     * ({@code pool[0][0..reqLen]}) and apply the response-overflow
     * policy.  {@code wireFallback} supplies the equivalent wire bytes
     * lazily — only materialised when a permitted retry exceeds the
     * pool cap and must take the {@link VesperaBridge#dispatchBytes} path.
     */
    private static ByteBuffer dispatchViaPool(
            ByteBuffer[] pool, int reqLen, boolean retryOnOverflow) {
        boolean recorded = false;
        try {
            int n = VesperaBridge.dispatchDirect(pool[0], reqLen, pool[1]);
            if (n == Integer.MIN_VALUE) {
                throw responseExceedsTwoGiBException();
            }
            if (n < 0 && n != Integer.MIN_VALUE) {
                int required = -n;
                if (!retryOnOverflow) {
                    throw new BufferTooSmallException(required);
                }
                if (required > DIRECT_MAX_CAPACITY) {
                    // Response exceeds the pooled direct buffer's hard cap. Do NOT
                    // heap-buffer the whole response via dispatchBytes — that
                    // defeats streaming and risks an OOM spike on large downloads
                    // (a small/bodyless safe GET the SmartDispatch resolver routes
                    // here can still return gigabytes). Surface the overflow so the
                    // caller re-routes this request through response streaming.
                    throw new BufferTooSmallException(required);
                }
                pool[1] = ByteBuffer.allocateDirect(grownCapacity(required));
                n = VesperaBridge.dispatchDirect(pool[0], reqLen, pool[1]);
            }
            if (n == Integer.MIN_VALUE) {
                throw responseExceedsTwoGiBException();
            }
            if (n < 0 && n != Integer.MIN_VALUE) {
                // A second overflow is legitimate: the retry re-ran the
                // handler, and a non-deterministic handler may produce a
                // larger response this time.  Surface the new exact size
                // instead of retrying unboundedly.
                throw new BufferTooSmallException(-n);
            }
            if (n < 0) {
                throw new IllegalStateException(
                        "dispatchDirect protocol violation: return code " + n + " after retry");
            }
            ByteBuffer view = pool[1].asReadOnlyBuffer();
            view.position(0).limit(n);
            recordDirectPoolUse(pool, reqLen, n);
            recorded = true;
            return view;
        } finally {
            if (!recorded) {
                recordDirectPoolUse(pool, reqLen, 0);
            }
        }
    }

    static IllegalStateException responseExceedsTwoGiBException() {
        return new IllegalStateException(
                "dispatchDirect response exceeds 2 GiB and cannot be represented; use streaming dispatch");
    }
}
