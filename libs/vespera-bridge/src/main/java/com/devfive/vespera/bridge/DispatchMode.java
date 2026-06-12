package com.devfive.vespera.bridge;

/**
 * How {@link VesperaProxyController} dispatches an incoming HTTP
 * request through the Rust JNI bridge.
 *
 * <p>The autoconfigured default {@link DispatchModeResolver} since
 * vespera-bridge 0.2.0 is {@link SmartDispatchModeResolver}: small
 * bounded idempotent requests take {@link #DIRECT} (~2.2 µs), small
 * non-idempotent requests take {@link #SYNC} (~3.2 µs), everything
 * else falls back to {@link #BIDIRECTIONAL_STREAMING} (~24 µs).  The
 * Spring side stays transparent to the vespera Rust router either
 * way — the routes published in the generated {@code openapi.json}
 * are reached via the same URLs, regardless of whether the underlying
 * handler emits a small JSON body or streams a multi-gigabyte file.
 *
 * <p>Restore the pre-0.2.0 default (every request that may carry a
 * body streams both ways) with the conservative opt-out:
 * {@code vespera.bridge.dispatch-mode=bidirectional-streaming} →
 * {@link BidirectionalStreamingDispatchModeResolver}.  Users who
 * want a different policy (sync for small JSON RPC, async for heavy
 * I/O coordination, …) can register a custom
 * {@link DispatchModeResolver} bean — {@code @ConditionalOnMissingBean}
 * ensures the default is automatically disabled.
 */
public enum DispatchMode {
    /**
     * Synchronous dispatch via
     * {@link VesperaBridge#dispatchBytes(byte[])}.  Full request
     * body is materialised in memory before dispatch; full response
     * body is materialised before return.  Smallest overhead for
     * tiny request/response pairs (typical JSON RPC).
     */
    SYNC,

    /**
     * Asynchronous dispatch via
     * {@link VesperaBridge#dispatchAsync(java.util.concurrent.CompletableFuture, byte[])}.
     * Returns a {@link java.util.concurrent.CompletableFuture}
     * completed from a Tokio worker thread.  Useful when the
     * controller wants to coordinate multiple parallel dispatches.
     */
    ASYNC,

    /**
     * Response-streaming dispatch via
     * {@link VesperaBridge#dispatchStreamingWithHeader(byte[],
     *        java.util.function.Consumer, java.io.OutputStream)}.
     * Request body is materialised; response body streams
     * chunk-by-chunk into the servlet output stream.  Suitable for
     * large downloads + small uploads (file serving, video).
     */
    STREAMING,

    /**
     * Bidirectional streaming dispatch via
     * {@link VesperaBridge#dispatchFullStreamingWithHeader(byte[],
     *        java.util.function.Consumer, java.io.InputStream,
     *        java.io.OutputStream)}.
     * Both request and response bodies stream chunk-by-chunk.
     * Works correctly for every payload size (small requests are
     * processed as a single chunk).  Selected by
     * {@link SmartDispatchModeResolver} (the autoconfigured default
     * since 0.2.0) for large or unknown-length bodies, and
     * unconditionally by the conservative opt-out
     * {@link BidirectionalStreamingDispatchModeResolver}
     * ({@code vespera.bridge.dispatch-mode=bidirectional-streaming},
     * pre-0.2.0 default).
     */
    BIDIRECTIONAL_STREAMING,

    /**
     * Direct-buffer dispatch via
     * {@link VesperaBridge#dispatchDirectPooled(byte[], boolean)} —
     * eliminates the JNI region copies and per-call Java heap array
     * allocations of {@link #SYNC}.
     *
     * <p>Selected by the autoconfigured
     * {@link SmartDispatchModeResolver} (default since 0.2.0) for
     * small, bounded, idempotent requests (GET/HEAD/PUT/DELETE/
     * OPTIONS with {@code Content-Length} absent or &le; 256 KiB).
     * The idempotency gate matters because a response that overflows
     * the pooled direct buffer re-runs the Rust handler once.  Never
     * selected by the conservative opt-out
     * {@link BidirectionalStreamingDispatchModeResolver}; large or
     * unbounded bodies always belong on
     * {@link #BIDIRECTIONAL_STREAMING}.
     */
    DIRECT,
}
