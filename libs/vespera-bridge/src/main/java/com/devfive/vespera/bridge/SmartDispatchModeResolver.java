package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Opt-in {@link DispatchModeResolver} that picks the cheapest safe
 * JNI path per request (measured on a small {@code GET /health}
 * round-trip: DIRECT 2.2&nbsp;µs / SYNC 3.2&nbsp;µs / bidirectional
 * streaming 24.1&nbsp;µs):
 *
 * <ul>
 *   <li>{@link DispatchMode#DIRECT} — safe requests
 *       (GET / HEAD / OPTIONS per RFC 9110) up to the DIRECT gate
 *       ({@link #DEFAULT_MAX_DIRECT_BYTES}, 1 MiB), or provably
 *       bodyless ones of any declared length.  Safety matters because
 *       a DIRECT response overflow retries the dispatch, re-running the
 *       Rust handler.</li>
 *   <li>{@link DispatchMode#SYNC} — unsafe requests
 *       (POST / PUT / PATCH / DELETE) up to the SYNC gate
 *       ({@link #DEFAULT_MAX_SYNC_BYTES}, 256 KiB).  SYNC never re-runs
 *       the handler, so it is safe for any method, but it fully buffers
 *       the response on the heap — so its gate is kept lower than the
 *       DIRECT gate, above which streaming wins.</li>
 *   <li>{@link DispatchMode#BIDIRECTIONAL_STREAMING} — everything
 *       else (larger or unknown-length bodies).</li>
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

    private static final String CURRENT_THREAD_IS_VIRTUAL_ATTRIBUTE =
            SmartDispatchModeResolver.class.getName() + ".currentThreadIsVirtual";

    /**
     * Default DIRECT request-size gate: 1 MiB (raised from 256 KiB,
     * measured 2026-06).  Safe requests up to this size dispatch
     * through pooled direct buffers — measured 1.7&ndash;2.7&times; faster
     * than streaming for 256 KiB&ndash;1 MiB bodies, provided
     * {@code vespera.direct.maxRetainedBytes} (2 MiB default) keeps the
     * response buffer resident so DIRECT does not re-run the handler.
     */
    public static final long DEFAULT_MAX_DIRECT_BYTES = 1024 * 1024L;

    /**
     * Default SYNC request-size gate: 256 KiB.  Unsafe (POST/PUT/PATCH/DELETE)
     * requests up to this size use SYNC; above it they stream, because
     * SYNC fully buffers the response on the JVM heap, which loses to
     * streaming for larger bodies (measured: SYNC 174 µs vs streaming
     * 83 µs at 1 MiB).  Kept lower than {@link #DEFAULT_MAX_DIRECT_BYTES}
     * on purpose — SYNC and DIRECT scale differently with size.
     */
    public static final long DEFAULT_MAX_SYNC_BYTES = 256 * 1024L;

    private final long maxDirectBytes;
    private final long maxSyncBytes;

    public SmartDispatchModeResolver() {
        this(DEFAULT_MAX_DIRECT_BYTES, DEFAULT_MAX_SYNC_BYTES);
    }

    /**
     * Single-gate constructor — sets BOTH the DIRECT and SYNC gates to
     * {@code maxDirectBytes} (the pre-split behavior).  Prefer
     * {@link #SmartDispatchModeResolver(long, long)} to gate DIRECT and
     * SYNC independently.
     *
     * @param maxDirectBytes largest {@code Content-Length} (bytes) eligible
     *                       for DIRECT (and, here, SYNC) dispatch
     */
    public SmartDispatchModeResolver(long maxDirectBytes) {
        this(maxDirectBytes, maxDirectBytes);
    }

    /**
     * @param maxDirectBytes largest {@code Content-Length} eligible for
     *                       DIRECT dispatch (safe methods)
     * @param maxSyncBytes   largest {@code Content-Length} eligible for SYNC
     *                       dispatch (unsafe methods); typically
     *                       lower than {@code maxDirectBytes}
     */
    public SmartDispatchModeResolver(long maxDirectBytes, long maxSyncBytes) {
        if (maxDirectBytes < 0 || maxSyncBytes < 0) {
            throw new IllegalArgumentException("byte gates must be >= 0");
        }
        this.maxDirectBytes = maxDirectBytes;
        this.maxSyncBytes = maxSyncBytes;
    }

    @Override
    public DispatchMode resolveMode(HttpServletRequest request) {
        return resolveMode(request, null);
    }

    static Boolean cachedCurrentThreadIsVirtual(HttpServletRequest request) {
        Object value = request.getAttribute(CURRENT_THREAD_IS_VIRTUAL_ATTRIBUTE);
        return value instanceof Boolean ? (Boolean) value : null;
    }

    DispatchMode resolveMode(HttpServletRequest request, boolean currentThreadIsVirtual) {
        return resolveMode(request, Boolean.valueOf(currentThreadIsVirtual));
    }

    private DispatchMode resolveMode(HttpServletRequest request, Boolean currentThreadIsVirtual) {
        long contentLength = request.getContentLengthLong();
        // Bodyless requests fit the direct buffer by definition even when
        // Content-Length is absent (the common shape of GET) — without this,
        // every length-less GET would miss the fast path.
        boolean bodyless = DispatchModeResolver.definitelyBodyless(request);
        String method = request.getMethod();

        if (HttpMethods.isSafe(method)) {
            // Safe (GET/HEAD/OPTIONS): DIRECT up to the (larger) DIRECT gate,
            // else stream.  Safety matters because a DIRECT response overflow
            // re-runs the Rust handler.
            boolean directSized =
                    bodyless || (contentLength >= 0 && contentLength <= maxDirectBytes);
            if (!directSized) {
                return DispatchMode.BIDIRECTIONAL_STREAMING;
            }
            // DIRECT's pooled direct buffers bind to the virtual thread (not
            // the carrier) in Java 21+, so on a virtual-thread-per-request
            // server dispatchDirectPooled allocates fresh off-heap buffers and
            // falls back to the heap path anyway.  Route virtual threads to
            // SYNC (no off-heap pooling, no re-run) when small, but stream
            // above the SYNC gate — SYNC's heap buffering loses to streaming
            // for larger bodies, idempotent or not.
            boolean virtualThread = currentThreadIsVirtual != null
                    ? currentThreadIsVirtual.booleanValue()
                    : VesperaBridge.currentThreadIsVirtual();
            request.setAttribute(CURRENT_THREAD_IS_VIRTUAL_ATTRIBUTE, Boolean.valueOf(virtualThread));
            if (virtualThread) {
                return syncSized(contentLength, bodyless)
                        ? DispatchMode.SYNC
                        : DispatchMode.BIDIRECTIONAL_STREAMING;
            }
            return DispatchMode.DIRECT;
        }

        // Unsafe (POST/PUT/PATCH/DELETE): SYNC never re-runs the handler, but
        // fully buffers the response on the JVM heap — which loses to
        // streaming above the (lower) SYNC gate.
        return syncSized(contentLength, bodyless)
                ? DispatchMode.SYNC
                : DispatchMode.BIDIRECTIONAL_STREAMING;
    }

    /** Whether a request fits the SYNC gate (bodyless or within the cap). */
    private boolean syncSized(long contentLength, boolean bodyless) {
        return bodyless || (contentLength >= 0 && contentLength <= maxSyncBytes);
    }
}
