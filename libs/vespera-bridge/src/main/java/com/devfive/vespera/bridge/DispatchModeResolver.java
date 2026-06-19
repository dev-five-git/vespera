package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Strategy for deciding which {@link DispatchMode} should serve an
 * incoming HTTP request.
 *
 * <p>The autoconfigured default since vespera-bridge 0.2.0 is
 * {@link SmartDispatchModeResolver}: small bounded safe
 * requests take {@link DispatchMode#DIRECT} (~2.2 µs), small
 * unsafe requests take {@link DispatchMode#SYNC} (~3.2 µs),
 * everything else falls back to
 * {@link DispatchMode#BIDIRECTIONAL_STREAMING} (~24 µs).  Spring
 * endpoints stay aligned with the URLs published in vespera's
 * {@code openapi.json} either way — the mode is picked per request
 * from request properties, not from the URL.
 *
 * <p>Restore the pre-0.2.0 default (every request that may carry a
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
     *   <li>HTTP/1.x only: no {@code Content-Length}, no
     *       {@code Transfer-Encoding}, and the method is GET / HEAD /
     *       OPTIONS — per RFC 9112 §6.3 such a request has no body.
     *       HTTP/2 is deliberately excluded because length-less DATA frames
     *       can carry a GET body and h2 has no {@code Transfer-Encoding}
     *       header.</li>
     * </ul>
     *
     * <p>For protocols other than HTTP/1.x, absence of framing headers is
     * treated as unknown rather than empty; callers that choose a
     * non-bidirectional mode will still read the servlet input stream fully.
     */
    static boolean definitelyBodyless(HttpServletRequest request) {
        // A `Transfer-Encoding` request frames its body by chunking, not by
        // Content-Length, and a malformed request carrying BOTH
        // `Content-Length: 0` and `Transfer-Encoding: chunked` is a classic
        // request-smuggling shape. Check TE FIRST so such a request is never
        // mistaken for bodyless — the prior order trusted `Content-Length: 0`
        // before ever looking at Transfer-Encoding.
        if (request.getHeader("Transfer-Encoding") != null) {
            return false;
        }
        long contentLength = request.getContentLengthLong();
        if (contentLength == 0) {
            return true;
        }
        if (contentLength > 0) {
            return false;
        }
        String protocol = request.getProtocol();
        if (protocol == null || !protocol.regionMatches(true, 0, "HTTP/1.", 0, 7)) {
            return false;
        }
        String method = request.getMethod();
        return "GET".equalsIgnoreCase(method)
                || "HEAD".equalsIgnoreCase(method)
                || "OPTIONS".equalsIgnoreCase(method);
    }
}
