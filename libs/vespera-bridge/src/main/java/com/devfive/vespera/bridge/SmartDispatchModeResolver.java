package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Opt-in {@link DispatchModeResolver} that picks the cheapest safe
 * JNI path per request (measured on a small {@code GET /health}
 * round-trip: DIRECT 2.2&nbsp;µs / SYNC 3.2&nbsp;µs / bidirectional
 * streaming 24.1&nbsp;µs):
 *
 * <ul>
 *   <li>{@link DispatchMode#STREAMING} — requests whose {@code Accept}
 *       names a streaming-only media type ({@code text/event-stream},
 *       {@code application/x-ndjson}, {@code application/stream+json}) and
 *       whose body fits the SYNC gate: the response is a long-lived stream,
 *       which a buffered mode would hold back until it ends.  Larger
 *       bodies take {@link DispatchMode#BIDIRECTIONAL_STREAMING}.</li>
 *   <li>{@link DispatchMode#DIRECT} — safe requests
 *       (GET / HEAD / OPTIONS per RFC 9110) up to the DIRECT gate
 *       ({@link #DEFAULT_MAX_DIRECT_BYTES}, 1 MiB), or provably
 *       bodyless ones of any declared length, on a platform thread.  A
 *       response that overflows the pooled buffer is kept in the native
 *       stash and fetched into a grown buffer, or — beyond the pool cap,
 *       or a {@code text/event-stream} — streamed from the stash; the
 *       handler runs once.</li>
 *   <li>{@link DispatchMode#STREAMING} — the same safe requests on a
 *       <em>virtual</em> thread.  The virtual-thread buffered path (pooled
 *       buffers bind to the virtual thread, so DIRECT falls back to the
 *       heap {@code dispatchBytes} route there) has no stash: a large
 *       download would fail at {@code max-buffered-response-bytes} and an
 *       event stream without {@code Accept: text/event-stream} would never
 *       flush.  Response streaming keeps DIRECT's semantics, at the cost of
 *       pinning the carrier while the handler runs (a native call;
 *       measured ~7.7&nbsp;µs vs ~3.2&nbsp;µs SYNC for a tiny
 *       {@code GET}).  {@link #SmartDispatchModeResolver(long, long, boolean)}
 *       with {@code parkSafeRequestsOnVirtualThreads = true} restores the
 *       parking SYNC route (never pins across a wait, but buffers the whole
 *       response) for safe requests within the SYNC gate.</li>
 *   <li>{@link DispatchMode#SYNC} — unsafe requests
 *       (POST / PUT / PATCH / DELETE) up to the SYNC gate
 *       ({@link #DEFAULT_MAX_SYNC_BYTES}, 256 KiB).  SYNC never re-runs
 *       the handler, so it is safe for any method, but it fully buffers
 *       the response on the heap — so its gate is kept lower than the
 *       DIRECT gate, above which streaming wins.  The Spring proxy also
 *       enforces {@code vespera.bridge.max-buffered-response-bytes}
 *       (64 MiB default) before serving a SYNC response.  On a virtual
 *       thread SYNC parks instead of pinning ({@link ParkingDispatch}).</li>
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

    /**
     * Default DIRECT request-size gate: 1 MiB (raised from 256 KiB,
     * measured 2026-06).  Safe requests up to this size dispatch
     * through pooled direct buffers — measured 1.7&ndash;2.7&times; faster
     * than streaming for 256 KiB&ndash;1 MiB bodies, provided
     * {@code vespera.direct.maxRetainedBytes} (2 MiB default) keeps the
     * response buffer resident, so an overflow (fetched from the native
     * stash into a grown buffer) stays rare.
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
    private final boolean parkSafeRequestsOnVirtualThreads;

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
        this(maxDirectBytes, maxSyncBytes, false);
    }

    /**
     * @param maxDirectBytes largest {@code Content-Length} eligible for
     *                       DIRECT dispatch (safe methods; on a virtual
     *                       thread: response-only STREAMING)
     * @param maxSyncBytes   largest {@code Content-Length} eligible for SYNC
     *                       dispatch (unsafe methods)
     * @param parkSafeRequestsOnVirtualThreads {@code true}: on a virtual
     *                       thread, safe requests within the SYNC gate take
     *                       the parking SYNC route (never pins the carrier
     *                       across a wait, but buffers the whole response —
     *                       {@code max-buffered-response-bytes} applies and
     *                       an event stream is only sent when it ends) and
     *                       larger ones stream both ways; {@code false}
     *                       (default): they stream the response, like a
     *                       DIRECT overflow on a platform thread
     */
    public SmartDispatchModeResolver(
            long maxDirectBytes, long maxSyncBytes, boolean parkSafeRequestsOnVirtualThreads) {
        if (maxDirectBytes < 0 || maxSyncBytes < 0) {
            throw new IllegalArgumentException("byte gates must be >= 0");
        }
        this.maxDirectBytes = maxDirectBytes;
        this.maxSyncBytes = maxSyncBytes;
        this.parkSafeRequestsOnVirtualThreads = parkSafeRequestsOnVirtualThreads;
    }

    @Override
    public DispatchMode resolveMode(HttpServletRequest request) {
        return resolveMode(request, null);
    }

    DispatchMode resolveMode(HttpServletRequest request, boolean currentThreadIsVirtual) {
        return resolveMode(request, Boolean.valueOf(currentThreadIsVirtual));
    }

    private DispatchMode resolveMode(HttpServletRequest request, Boolean currentThreadIsVirtual) {
        RequestShape shape = RequestShape.from(request);
        long contentLength = shape.contentLength;
        // Bodyless requests fit the direct buffer by definition even when
        // Content-Length is absent (the common shape of GET) — without this,
        // every length-less GET would miss the fast path.
        boolean bodyless = shape.definitelyBodyless;
        String method = shape.method;

        if (acceptsStreaming(request.getHeader("Accept"))) {
            // SSE / NDJSON: response-stream it so events reach the client as
            // they are produced (buffered modes would hold them until the
            // stream ends — forever for a long-lived event stream).
            return syncSized(contentLength, bodyless)
                    ? DispatchMode.STREAMING
                    : DispatchMode.BIDIRECTIONAL_STREAMING;
        }

        if (HttpMethods.isSafe(method)) {
            // Safe (GET/HEAD/OPTIONS): DIRECT up to the (larger) DIRECT gate,
            // else stream.  A DIRECT response overflow is fetched or streamed
            // from the native stash (the handler runs once).
            boolean directSized =
                    bodyless || (contentLength >= 0 && contentLength <= maxDirectBytes);
            if (!directSized) {
                return DispatchMode.BIDIRECTIONAL_STREAMING;
            }
            // DIRECT's pooled direct buffers bind to the virtual thread (not
            // the carrier) in Java 21+, so on a virtual thread DIRECT falls
            // back to the buffered heap path, which has no stash: a large
            // download would hit max-buffered-response-bytes (500) and an
            // event stream without an SSE Accept would never flush.  Stream
            // the response instead (DIRECT's overflow semantics), unless the
            // application opted into the parking SYNC route.
            boolean virtualThread = currentThreadIsVirtual != null
                    ? currentThreadIsVirtual.booleanValue()
                    : VesperaDirectBufferPool.currentThreadIsVirtual();
            if (virtualThread) {
                if (!parkSafeRequestsOnVirtualThreads) {
                    return DispatchMode.STREAMING;
                }
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

    /**
     * Whether an {@code Accept} value names a streaming-only media type
     * ({@code text/event-stream}, {@code application/x-ndjson},
     * {@code application/stream+json}) with a non-zero quality.  A cheap
     * {@code "stream"}/{@code "ndjson"} probe keeps the common
     * {@code application/json} / {@code *}{@code /*} case allocation-free.
     */
    static boolean acceptsStreaming(String accept) {
        if (accept == null) {
            return false;
        }
        String lower = accept.toLowerCase(java.util.Locale.ROOT);
        if (!lower.contains("stream") && !lower.contains("ndjson")) {
            return false;
        }
        for (String range : lower.split(",")) {
            int semi = range.indexOf(';');
            String type = (semi < 0 ? range : range.substring(0, semi)).trim();
            if ((type.equals("text/event-stream")
                    || type.equals("application/x-ndjson")
                    || type.equals("application/stream+json"))
                    && !zeroQuality(semi < 0 ? "" : range.substring(semi))) {
                return true;
            }
        }
        return false;
    }

    private static boolean zeroQuality(String params) {
        for (String param : params.split(";")) {
            String p = param.trim();
            if (p.startsWith("q=")) {
                try {
                    return Double.parseDouble(p.substring(2).trim()) <= 0.0;
                } catch (NumberFormatException e) {
                    return false;
                }
            }
        }
        return false;
    }

    /** Whether a request fits the SYNC gate (bodyless or within the cap). */
    private boolean syncSized(long contentLength, boolean bodyless) {
        return bodyless || (contentLength >= 0 && contentLength <= maxSyncBytes);
    }
}
