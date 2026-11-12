package com.devfive.vespera.bridge;

import org.springframework.boot.context.properties.ConfigurationProperties;

import java.time.Duration;

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
 *     max-buffered-response-bytes: 67108864 # cap SYNC heap-buffered response bodies
 *     clear-threadlocals-after-request: false # clear per-thread buffers after each proxied request
 *     native:                     # Rust-side knobs, applied before the first dispatch
 *       worker-threads: 8
 *       max-blocking-threads: 256
 *       max-request-bytes: 67108864
 *       max-streaming-request-bytes: 0
 *       max-buffered-response-bytes: 268435456
 *       dispatch-timeout: 30s
 *       async-max-in-flight: 4096
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
     * Maximum response-body bytes the Spring proxy will serve from the
     * heap-buffered SYNC path. Default 64 MiB keeps SmartDispatch's small
     * unsafe fast path bounded; set {@code 0} to restore unlimited SYNC
     * response buffering. Large/unknown downloads should use streaming modes.
     */
    private long maxBufferedResponseBytes = VesperaProxyController.DEFAULT_MAX_BUFFERED_RESPONSE_BYTES;

    /**
     * Thread count for the autoconfigured {@code vesperaBridgeAsyncResponseExecutor}
     * — the JVM-side pool that parses the ASYNC wire response off the native
     * completion thread.  Default {@code 0} preserves the historical sizing
     * ({@code Math.max(2, Math.min(4, availableProcessors()))}).  Set a positive
     * value to override the cap for high-concurrency async dispatch; the value
     * is clamped to at least {@code 1}.
     */
    private int asyncPoolSize = 0;

    /**
     * When true, an autoconfigured servlet filter clears vespera's per-thread
     * direct/header/scratch buffers in a {@code finally} block after every
     * request. Default false keeps hot servlet threads pooled for throughput;
     * enable for containers/redeploy setups where idle worker threads outlive
     * the Spring context and ThreadLocal retention is more important than reuse.
     */
    private boolean clearThreadlocalsAfterRequest = false;

    /**
     * Rust-side runtime knobs forwarded to {@link VesperaBridge}'s
     * {@code configure*} methods when the autoconfiguration starts.  Unset
     * values leave the native defaults (or sysprop/env overrides) in place.
     */
    private final Native nativeSettings = new Native();

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

    public long getMaxBufferedResponseBytes() {
        return maxBufferedResponseBytes;
    }

    public void setMaxBufferedResponseBytes(long maxBufferedResponseBytes) {
        this.maxBufferedResponseBytes = maxBufferedResponseBytes;
    }

    public int getAsyncPoolSize() {
        return asyncPoolSize;
    }

    public void setAsyncPoolSize(int asyncPoolSize) {
        this.asyncPoolSize = asyncPoolSize;
    }

    public boolean isClearThreadlocalsAfterRequest() {
        return clearThreadlocalsAfterRequest;
    }

    public void setClearThreadlocalsAfterRequest(boolean clearThreadlocalsAfterRequest) {
        this.clearThreadlocalsAfterRequest = clearThreadlocalsAfterRequest;
    }

    public Native getNative() {
        return nativeSettings;
    }

    /**
     * Rust-side knobs under {@code vespera.bridge.native.*}.  Every field is
     * nullable: {@code null} means "not configured here".  They only take
     * effect before the first native dispatch; a late value is logged and
     * ignored by {@link VesperaBridge}.
     */
    public static class Native {

        /** Shared Tokio runtime worker threads (1–1024). */
        private Integer workerThreads;

        /** Shared Tokio runtime blocking-pool cap (1–8192, native default 512). */
        private Integer maxBlockingThreads;

        /** Buffered request-body cap in bytes ({@code 0} = unlimited, native default 64 MiB). */
        private Long maxRequestBytes;

        /** Streamed request-body cap in bytes ({@code 0} = unlimited, the native default). */
        private Long maxStreamingRequestBytes;

        /** Buffered response-body cap in bytes ({@code 0} = unlimited, native default 256 MiB). */
        private Long maxBufferedResponseBytes;

        /** Per-dispatch deadline ({@code 0} = disabled, the native default). */
        private Duration dispatchTimeout;

        /** {@code dispatchAsync} in-flight cap ({@code 0} = unlimited, native default 4096). */
        private Integer asyncMaxInFlight;

        public Integer getWorkerThreads() {
            return workerThreads;
        }

        public void setWorkerThreads(Integer workerThreads) {
            this.workerThreads = workerThreads;
        }

        public Integer getMaxBlockingThreads() {
            return maxBlockingThreads;
        }

        public void setMaxBlockingThreads(Integer maxBlockingThreads) {
            this.maxBlockingThreads = maxBlockingThreads;
        }

        public Long getMaxRequestBytes() {
            return maxRequestBytes;
        }

        public void setMaxRequestBytes(Long maxRequestBytes) {
            this.maxRequestBytes = maxRequestBytes;
        }

        public Long getMaxStreamingRequestBytes() {
            return maxStreamingRequestBytes;
        }

        public void setMaxStreamingRequestBytes(Long maxStreamingRequestBytes) {
            this.maxStreamingRequestBytes = maxStreamingRequestBytes;
        }

        public Long getMaxBufferedResponseBytes() {
            return maxBufferedResponseBytes;
        }

        public void setMaxBufferedResponseBytes(Long maxBufferedResponseBytes) {
            this.maxBufferedResponseBytes = maxBufferedResponseBytes;
        }

        public Duration getDispatchTimeout() {
            return dispatchTimeout;
        }

        public void setDispatchTimeout(Duration dispatchTimeout) {
            this.dispatchTimeout = dispatchTimeout;
        }

        public Integer getAsyncMaxInFlight() {
            return asyncMaxInFlight;
        }

        public void setAsyncMaxInFlight(Integer asyncMaxInFlight) {
            this.asyncMaxInFlight = asyncMaxInFlight;
        }

        /**
         * Forward every configured value to {@link VesperaBridge}.  Unset
         * values are passed as that method's "leave untouched" sentinel
         * ({@code 0} for the runtime pair, {@code -1} otherwise).
         */
        void apply() {
            if (workerThreads != null || maxBlockingThreads != null) {
                VesperaBridge.configureRuntime(
                        workerThreads == null ? 0 : workerThreads,
                        maxBlockingThreads == null ? 0 : maxBlockingThreads);
            }
            if (maxRequestBytes != null || maxStreamingRequestBytes != null
                    || maxBufferedResponseBytes != null) {
                VesperaBridge.configureLimits(
                        maxRequestBytes == null ? -1 : maxRequestBytes,
                        maxStreamingRequestBytes == null ? -1 : maxStreamingRequestBytes,
                        maxBufferedResponseBytes == null ? -1 : maxBufferedResponseBytes);
            }
            if (dispatchTimeout != null) {
                VesperaBridge.configureDispatchTimeout(dispatchTimeout.toMillis());
            }
            if (asyncMaxInFlight != null) {
                VesperaBridge.configureAsyncMaxInFlight(asyncMaxInFlight);
            }
        }
    }
}
