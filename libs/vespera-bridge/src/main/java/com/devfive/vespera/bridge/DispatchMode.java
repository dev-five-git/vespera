package com.devfive.vespera.bridge;

/**
 * How {@link VesperaProxyController} dispatches an incoming HTTP
 * request through the Rust JNI bridge.
 *
 * <p>The default {@link DispatchModeResolver} returns
 * {@link #BIDIRECTIONAL_STREAMING} for every request so that the
 * Spring side stays transparent to the vespera Rust router — the
 * routes published in the generated {@code openapi.json} are reached
 * via the same URLs, regardless of whether the underlying handler
 * emits a small JSON body or streams a multi-gigabyte file.  Users
 * who want a different policy (sync for small JSON RPC, async for
 * heavy I/O coordination, …) can register a custom
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
     * This is the <strong>default mode</strong> — it works correctly
     * for every payload size (small requests are processed as a
     * single chunk), so callers see the vespera Rust router's
     * endpoints exactly as published in {@code openapi.json} with
     * no special configuration.
     */
    BIDIRECTIONAL_STREAMING,

    /**
     * Direct-buffer dispatch via
     * {@link VesperaBridge#dispatchDirectPooled(byte[], boolean)} —
     * eliminates the JNI region copies and per-call Java heap array
     * allocations of {@link #SYNC}.
     *
     * <p><strong>Opt-in only</strong> — never selected by the
     * autoconfigured default resolver.  Wire a
     * {@link SmartDispatchModeResolver} (or a custom resolver) to use
     * it.  Suitable for small, bounded payloads with a known
     * {@code Content-Length}; large or unbounded bodies belong on
     * {@link #BIDIRECTIONAL_STREAMING}.
     */
    DIRECT,
}
