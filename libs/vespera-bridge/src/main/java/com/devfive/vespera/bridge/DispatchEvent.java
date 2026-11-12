package com.devfive.vespera.bridge;

/**
 * One request proxied by {@link VesperaProxyController}, reported to every
 * {@link DispatchObserver} bean after the response was produced.
 *
 * @param app           target app ({@code "_default"} for the default app)
 * @param mode          dispatch mode used, or {@code null} when the request
 *                      failed before a mode was chosen (e.g. an unknown app)
 * @param method        HTTP method
 * @param path          forwarded (application-relative) path
 * @param status        response status (for streaming responses: the
 *                      committed status, even if the body later failed)
 * @param durationNanos wall time from entering the proxy until completion
 * @param panicked      whether Rust reported a caught panic
 *                      ({@code metadata.panicked})
 * @param requestId     request id ({@code vespera.bridge.request-id.*}), or
 *                      {@code null} when request ids are disabled
 */
public record DispatchEvent(
        String app,
        DispatchMode mode,
        String method,
        String path,
        int status,
        long durationNanos,
        boolean panicked,
        String requestId) {
}
