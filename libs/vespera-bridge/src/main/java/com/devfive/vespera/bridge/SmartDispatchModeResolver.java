package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

import java.util.Locale;
import java.util.Set;

/**
 * Opt-in {@link DispatchModeResolver} that routes small, bounded,
 * idempotent requests through {@link DispatchMode#DIRECT} and
 * everything else through {@link DispatchMode#BIDIRECTIONAL_STREAMING}.
 *
 * <p><strong>Not wired by default.</strong> The autoconfigured
 * resolver remains {@link BidirectionalStreamingDispatchModeResolver};
 * register this class as a {@code @Bean} to opt in:
 *
 * <pre>{@code
 * @Bean
 * public DispatchModeResolver dispatchModeResolver() {
 *     return new SmartDispatchModeResolver();
 * }
 * }</pre>
 *
 * <p>DIRECT is selected only when ALL of the following hold —
 * otherwise the request falls back to bidirectional streaming:
 * <ul>
 *   <li>{@code Content-Length} is known ({@code >= 0}; chunked
 *       transfer encoding has none) and within {@link #maxDirectBytes}
 *       — the request must fit the pooled direct buffer without
 *       streaming.</li>
 *   <li>The HTTP method is idempotent per RFC 9110 (GET / HEAD /
 *       PUT / DELETE / OPTIONS) — a DIRECT response overflow retries
 *       the dispatch, which re-runs the Rust handler, so
 *       non-idempotent methods (POST / PATCH) never use DIRECT.</li>
 * </ul>
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
        if (contentLength < 0 || contentLength > maxDirectBytes) {
            return DispatchMode.BIDIRECTIONAL_STREAMING;
        }
        String method = request.getMethod();
        if (method == null
                || !IDEMPOTENT_METHODS.contains(method.toUpperCase(Locale.ROOT))) {
            return DispatchMode.BIDIRECTIONAL_STREAMING;
        }
        return DispatchMode.DIRECT;
    }
}
