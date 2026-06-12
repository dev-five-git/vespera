package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Strategy for deciding which {@link DispatchMode} should serve an
 * incoming HTTP request.
 *
 * <p>The autoconfigured default since vespera-bridge 1.0.0 is
 * {@link SmartDispatchModeResolver}: small bounded idempotent
 * requests take {@link DispatchMode#DIRECT} (~2.2 µs), small
 * non-idempotent requests take {@link DispatchMode#SYNC} (~3.2 µs),
 * everything else falls back to
 * {@link DispatchMode#BIDIRECTIONAL_STREAMING} (~24 µs).  Spring
 * endpoints stay aligned with the URLs published in vespera's
 * {@code openapi.json} either way — the mode is picked per request
 * from request properties, not from the URL.
 *
 * <p>Restore the pre-1.0.0 default (every request that may carry a
 * body streams both ways) with the conservative opt-out:
 * {@code vespera.bridge.dispatch-mode=bidirectional-streaming} →
 * {@link BidirectionalStreamingDispatchModeResolver}.
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
