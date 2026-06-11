package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Strategy for deciding which {@link DispatchMode} should serve an
 * incoming HTTP request.
 *
 * <p>The autoconfigured default returns
 * {@link DispatchMode#BIDIRECTIONAL_STREAMING} for every request,
 * which works correctly across all payload sizes (small requests
 * are processed as a single chunk) and keeps Spring endpoints
 * aligned with the URLs published in vespera's {@code openapi.json}
 * — no path-based mode selection that would diverge from the Rust
 * router's view.
 *
 * <p>Users who want a mixed policy (e.g. {@link DispatchMode#SYNC}
 * for sub-KB JSON RPC, {@link DispatchMode#STREAMING} for paths
 * matching {@code /files/**}, {@link DispatchMode#ASYNC} for
 * everything else) can register a custom {@code DispatchModeResolver}
 * bean — the autoconfigure module's {@code @ConditionalOnMissingBean}
 * gate automatically disables the default.
 *
 * <p>Implementations must be safe to call from multiple servlet
 * threads concurrently.
 */
@FunctionalInterface
public interface DispatchModeResolver {

    /**
     * Pick the dispatch mode for the supplied request.
     *
     * @return non-null {@link DispatchMode} value
     */
    DispatchMode resolveMode(HttpServletRequest request);

    /**
     * {@code true} when the request provably carries no body, so the
     * bidirectional request-pull plumbing (a blocking pull thread, a
     * bounded channel, and per-chunk JNI crossings — measured at
     * ~16&nbsp;µs per request) would be pure overhead.
     *
     * <p>Detection is deliberately conservative:
     * <ul>
     *   <li>{@code Content-Length: 0} — provably empty for any method
     *       and protocol.</li>
     *   <li>No {@code Content-Length}, no {@code Transfer-Encoding},
     *       and the method is GET / HEAD / OPTIONS — per RFC 9112
     *       §6.3 such an HTTP/1.1 request has no body.  The method
     *       restriction keeps HTTP/2 safe (h2 has no
     *       {@code Transfer-Encoding} header, so a length-less POST
     *       body cannot be ruled out there).</li>
     * </ul>
     *
     * <p>Even when this misjudges an exotic length-less GET-with-body
     * (h2 only), correctness is preserved — the non-bidirectional
     * modes read the servlet input stream fully and send the body
     * inline; only the memory profile differs.
     */
    static boolean definitelyBodyless(HttpServletRequest request) {
        long contentLength = request.getContentLengthLong();
        if (contentLength == 0) {
            return true;
        }
        if (contentLength > 0 || request.getHeader("Transfer-Encoding") != null) {
            return false;
        }
        String method = request.getMethod();
        return "GET".equalsIgnoreCase(method)
                || "HEAD".equalsIgnoreCase(method)
                || "OPTIONS".equalsIgnoreCase(method);
    }
}
