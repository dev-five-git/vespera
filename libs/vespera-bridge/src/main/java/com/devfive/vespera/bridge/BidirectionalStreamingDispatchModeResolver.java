package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Default {@link DispatchModeResolver} — bidirectional streaming for
 * every request that may carry a body, with one semantics-preserving
 * fast path: provably bodyless requests (see
 * {@link DispatchModeResolver#definitelyBodyless}) use response-only
 * {@link DispatchMode#STREAMING}, skipping the request-pull plumbing
 * that costs ~16&nbsp;µs per request even when there is nothing to
 * pull (measured 24.1&nbsp;µs → 7.7&nbsp;µs on a small GET).
 *
 * <p>This remains the safest universal default: every payload size
 * is processed correctly (responses always stream chunk-bounded;
 * request bodies stream whenever one can exist), and the Spring
 * endpoints exactly mirror the URLs in vespera's generated
 * {@code openapi.json}.  No path-based mode discrimination means no
 * surprise divergence from the Rust router's view.
 *
 * <p>Replace this with a custom {@link DispatchModeResolver} bean if
 * your application needs different modes for different routes
 * (e.g. sync for sub-KB JSON RPC, async for parallel I/O
 * coordination) — or to restore unconditional bidirectional
 * streaming with a one-line lambda.
 */
public final class BidirectionalStreamingDispatchModeResolver
        implements DispatchModeResolver {

    @Override
    public DispatchMode resolveMode(HttpServletRequest request) {
        return DispatchModeResolver.definitelyBodyless(request)
                ? DispatchMode.STREAMING
                : DispatchMode.BIDIRECTIONAL_STREAMING;
    }
}
