package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Opt-in {@link DispatchModeResolver} that picks the cheapest safe
 * JNI path per request (measured on a small {@code GET /health}
 * round-trip: DIRECT 2.2&nbsp;µs / SYNC 3.2&nbsp;µs / bidirectional
 * streaming 24.1&nbsp;µs):
 *
 * <ul>
 *   <li>{@link DispatchMode#DIRECT} — small bounded
 *       (<= {@link #maxDirectBytes}) or provably bodyless requests
 *       with an idempotent method (GET / HEAD / PUT / DELETE /
 *       OPTIONS per RFC 9110).  Idempotency matters because a DIRECT
 *       response overflow retries the dispatch, re-running the Rust
 *       handler.</li>
 *   <li>{@link DispatchMode#SYNC} — small bounded requests with a
 *       non-idempotent method (POST / PATCH).  SYNC never re-runs
 *       the handler, so it is safe for any method; the response is
 *       fully buffered on the heap, which the size gate keeps
 *       reasonable for JSON-RPC-shaped traffic.</li>
 *   <li>{@link DispatchMode#BIDIRECTIONAL_STREAMING} — everything
 *       else (large or unknown-length bodies).</li>
 * </ul>
 *
 * <p><strong>Autoconfigured default since vespera-bridge 0.2.0.</strong>
 * No property required — the autoconfigure module wires this resolver
 * when no user {@code @Bean DispatchModeResolver} exists.  Pin it
 * explicitly with {@code vespera.bridge.dispatch-mode=smart}, or
 * opt out to the pre-0.2.0 conservative default with
 * {@code vespera.bridge.dispatch-mode=bidirectional-streaming} →
 * {@link BidirectionalStreamingDispatchModeResolver}.  Or register a
 * custom resolver — {@code @ConditionalOnMissingBean} guarantees it
 * wins over both:
 *
 * <pre>{@code
 * @Bean
 * public DispatchModeResolver dispatchModeResolver() {
 *     return new SmartDispatchModeResolver();
 * }
 * }</pre>
 */
public class SmartDispatchModeResolver implements DispatchModeResolver {

    /** Default request-size gate: 256 KiB. */
    public static final long DEFAULT_MAX_DIRECT_BYTES = 256 * 1024L;

    private final long maxDirectBytes;

    public SmartDispatchModeResolver() {
        this(DEFAULT_MAX_DIRECT_BYTES);
    }

    /**
     * @param maxDirectBytes largest {@code Content-Length} (bytes)
     *                       eligible for DIRECT dispatch
     */
    public SmartDispatchModeResolver(long maxDirectBytes) {
        if (maxDirectBytes < 0) {
            throw new IllegalArgumentException("maxDirectBytes must be >= 0");
        }
        this.maxDirectBytes = maxDirectBytes;
    }

    @Override
    public DispatchMode resolveMode(HttpServletRequest request) {
        long contentLength = request.getContentLengthLong();
        boolean smallBounded = contentLength >= 0 && contentLength <= maxDirectBytes;
        // Bodyless requests fit the direct buffer by definition even
        // when Content-Length is absent (the common shape of GET) —
        // without this, every length-less GET missed the fast path.
        boolean directSized =
                smallBounded || DispatchModeResolver.definitelyBodyless(request);
        if (!directSized) {
            return DispatchMode.BIDIRECTIONAL_STREAMING;
        }
        String method = request.getMethod();
        if (HttpMethods.isIdempotent(method)) {
            // DIRECT's pooled direct buffers bind to the virtual thread
            // (not the carrier) in Java 21+, so on a virtual-thread-per-
            // request server dispatchDirectPooled allocates fresh off-heap
            // buffers and falls back to the heap path anyway.  Route
            // virtual threads straight to SYNC to skip the direct-buffer
            // machinery; the request is already direct-sized (small or
            // bodyless) and SYNC never re-runs the handler, so it is safe.
            if (VesperaBridge.currentThreadIsVirtual()) {
                return DispatchMode.SYNC;
            }
            return DispatchMode.DIRECT;
        }
        // Small non-idempotent (POST / PATCH): SYNC never re-runs the
        // handler — 7.5x cheaper than bidirectional for small bodies.
        return smallBounded ? DispatchMode.SYNC : DispatchMode.BIDIRECTIONAL_STREAMING;
    }
}
