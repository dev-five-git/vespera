package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

import java.util.Locale;
import java.util.Set;

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
 * <p><strong>Not wired by default.</strong> The autoconfigured
 * resolver remains {@link BidirectionalStreamingDispatchModeResolver};
 * opt in via {@code vespera.bridge.dispatch-mode=smart} or register
 * this class as a {@code @Bean}:
 *
 * <pre>{@code
 * @Bean
 * public DispatchModeResolver dispatchModeResolver() {
 *     return new SmartDispatchModeResolver();
 * }
 * }</pre>
 */
public class SmartDispatchModeResolver implements DispatchModeResolver {

    private static final Set<String> IDEMPOTENT_METHODS =
            Set.of("GET", "HEAD", "PUT", "DELETE", "OPTIONS");

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
        if (method != null && IDEMPOTENT_METHODS.contains(method.toUpperCase(Locale.ROOT))) {
            return DispatchMode.DIRECT;
        }
        // Small non-idempotent (POST / PATCH): SYNC never re-runs the
        // handler — 7.5x cheaper than bidirectional for small bodies.
        return smallBounded ? DispatchMode.SYNC : DispatchMode.BIDIRECTIONAL_STREAMING;
    }
}
