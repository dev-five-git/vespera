package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Default {@link DispatchModeResolver} — always returns
 * {@link DispatchMode#BIDIRECTIONAL_STREAMING}.
 *
 * <p>This is the safest universal default: every payload size
 * (including 0-byte requests and tiny JSON bodies) is processed
 * correctly through the bidirectional streaming JNI path, and the
 * Spring endpoints exactly mirror the URLs in vespera's generated
 * {@code openapi.json}.  No path-based mode discrimination means no
 * surprise divergence from the Rust router's view.
 *
 * <p>Replace this with a custom {@link DispatchModeResolver} bean if
 * your application needs different modes for different routes
 * (e.g. sync for sub-KB JSON RPC, async for parallel I/O
 * coordination).
 */
public final class BidirectionalStreamingDispatchModeResolver
        implements DispatchModeResolver {

    @Override
    public DispatchMode resolveMode(HttpServletRequest request) {
        return DispatchMode.BIDIRECTIONAL_STREAMING;
    }
}
