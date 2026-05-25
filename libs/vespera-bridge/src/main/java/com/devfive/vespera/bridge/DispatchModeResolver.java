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
}
