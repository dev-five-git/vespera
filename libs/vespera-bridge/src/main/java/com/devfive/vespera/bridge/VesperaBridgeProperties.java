package com.devfive.vespera.bridge;

import org.springframework.boot.context.properties.ConfigurationProperties;

/**
 * Properties for the autoconfigured vespera-bridge defaults.
 * Prefix: {@code vespera.bridge}.
 *
 * <p>Defaults are tuned for the "transparent wrapping" use-case:
 * routes published in vespera's {@code openapi.json} are reachable
 * at the same URLs through Spring, with no app-selection prefix or
 * mode-selection prefix.  Override individual properties to tweak
 * the defaults, or register a custom
 * {@link AppNameResolver} / {@link DispatchModeResolver} bean to
 * fully replace the resolver logic.
 *
 * <pre>{@code
 * vespera:
 *   bridge:
 *     app-header: X-My-App        # override the default header name
 *     controller-enabled: false   # disable our controller (BYO controller)
 *     direct-retry-on-overflow: false # surface DIRECT overflow instead of retrying
 *     max-buffered-request-bytes: 10485760 # cap SYNC/ASYNC/DIRECT/STREAMING request buffering
 * }</pre>
 */
@ConfigurationProperties(prefix = "vespera.bridge")
public class VesperaBridgeProperties {

    /**
     * HTTP request header inspected by the default
     * {@link HeaderAppNameResolver} to pick the target app for
     * multi-app routing.  Default: {@code X-Vespera-App}.  When the
     * header is absent on a given request, that request is routed
     * to the default app registered via {@code register_app}.
     */
    private String appHeader = "X-Vespera-App";

    /**
     * When {@code true} (default), Spring Boot autoconfigures
     * {@link VesperaProxyController} as a catch-all proxy.  Set to
     * {@code false} when you want to provide your own controller —
     * the {@link VesperaBridge} native methods remain available
     * for direct use either way.
     */
    private boolean controllerEnabled = true;

    /**
     * Dispatch-mode policy for the autoconfigured proxy.
     *
     * <ul>
     *   <li>{@code smart} (default since 0.2.0) — small bounded safe
     *       requests (Content-Length absent/bodyless or &le; 1 MiB;
     *       GET/HEAD/OPTIONS) take the pooled
     *       direct-buffer path, skipping JNI array copies and
     *       per-request stream setup; small unsafe requests
     *       (POST/PUT/PATCH/DELETE) take heap-buffered SYNC; everything else
     *       falls back to bidirectional streaming.  Measured 2.2 µs
     *       (DIRECT) / 3.2 µs (SYNC) vs 24.1 µs (bidirectional) on
     *       a small {@code GET /health} round-trip.  Trade-offs:
     *       DIRECT re-runs the handler when a response overflows the
     *       pooled buffer ({@code vespera.direct.maxBufferBytes},
     *       default 4 MiB) — acceptable for safe requests
     *       only; SYNC fully buffers the response on the JVM heap.</li>
     *   <li>{@code bidirectional-streaming} — opt-out, restores the
     *       pre-0.2.0 default: every request that may carry a body
     *       streams both ways; safe for any payload size; the
     *       uniform per-request cost is ~24 µs even on small
     *       JSON-RPC payloads.</li>
     * </ul>
     */
    private String dispatchMode = "smart";

    /**
     * Whether the Spring proxy may retry a DIRECT response-buffer overflow
     * for safe methods.  Default {@code true} preserves the 0.2.x
     * behavior (grow the direct response buffer once and re-run the Rust
     * handler). Set {@code false} to surface
     * {@link VesperaBridge.BufferTooSmallException} as a 500 instead,
     * avoiding any automatic double execution.
     */
    private boolean directRetryOnOverflow = true;

    /**
     * Maximum request-body bytes the Spring proxy may buffer for
     * SYNC/ASYNC/DIRECT/STREAMING dispatch modes.  The conservative default is
     * 64 MiB so a custom resolver cannot accidentally route an unknown-length
     * upload into a heap-buffered mode and grow toward the JVM array ceiling.
     * Set {@code 0} explicitly to restore unlimited buffering. Bidirectional
     * streaming is exempt because it does not fully buffer the request body.
     */
    private long maxBufferedRequestBytes = VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES;

    /**
     * Thread count for the autoconfigured {@code vesperaBridgeAsyncResponseExecutor}
     * — the JVM-side pool that parses the ASYNC wire response off the native
     * completion thread.  Default {@code 0} preserves the historical sizing
     * ({@code Math.max(2, Math.min(4, availableProcessors()))}).  Set a positive
     * value to override the cap for high-concurrency async dispatch; the value
     * is clamped to at least {@code 1}.
     */
    private int asyncPoolSize = 0;

    public String getAppHeader() {
        return appHeader;
    }

    public void setAppHeader(String appHeader) {
        this.appHeader = appHeader;
    }

    public boolean isControllerEnabled() {
        return controllerEnabled;
    }

    public void setControllerEnabled(boolean controllerEnabled) {
        this.controllerEnabled = controllerEnabled;
    }

    public String getDispatchMode() {
        return dispatchMode;
    }

    public void setDispatchMode(String dispatchMode) {
        this.dispatchMode = dispatchMode;
    }

    public boolean isDirectRetryOnOverflow() {
        return directRetryOnOverflow;
    }

    public void setDirectRetryOnOverflow(boolean directRetryOnOverflow) {
        this.directRetryOnOverflow = directRetryOnOverflow;
    }

    public long getMaxBufferedRequestBytes() {
        return maxBufferedRequestBytes;
    }

    public void setMaxBufferedRequestBytes(long maxBufferedRequestBytes) {
        this.maxBufferedRequestBytes = maxBufferedRequestBytes;
    }

    public int getAsyncPoolSize() {
        return asyncPoolSize;
    }

    public void setAsyncPoolSize(int asyncPoolSize) {
        this.asyncPoolSize = asyncPoolSize;
    }
}
