package com.devfive.vespera.bridge;

import org.springframework.boot.context.properties.ConfigurationProperties;

import java.time.Duration;
import java.util.ArrayList;
import java.util.List;

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
 *     exposed-apps: [admin]       # apps the client-controlled app header may select
 *     controller-enabled: false   # disable our controller (BYO controller)
 *     include-patterns: [/api/**] # paths the proxy owns (default /**)
 *     exclude-patterns: [/api/internal/**] # left to Spring MVC (handlers, static resources, 404)
 *     direct-retry-on-overflow: false # surface DIRECT overflow instead of retrying
 *     max-buffered-request-bytes: 10485760 # cap SYNC/ASYNC/DIRECT/STREAMING request buffering
 *     max-buffered-response-bytes: 67108864 # cap SYNC heap-buffered response bodies
 *     clear-threadlocals-after-request: false # clear per-thread buffers after each proxied request
 *     library-name: my_app        # auto-run VesperaBridge.init("my_app") at startup
 *     shutdown-on-close: false    # shut the native runtime down on context close (irreversible)
 *     shutdown-timeout: 10s
 *     request-id:
 *       enabled: true             # read / generate, forward, echo and MDC the request id
 *       header: X-Request-Id
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
     * App names a request may select through {@link #appHeader} (the
     * {@code _default} app is always selectable).  <strong>Security:</strong>
     * the header is client-controlled — while this list is empty any client
     * can route any URL into any registered app (e.g. {@code GET /users} with
     * {@code X-Vespera-App: admin}), bypassing path-based Spring Security
     * rules, and the proxy logs a one-time WARN on the first non-default
     * selection.  When set, any other app name is answered with {@code 404}
     * without dispatching.  Only the default {@link HeaderAppNameResolver}
     * honours it; a custom {@link AppNameResolver} enforces its own policy.
     */
    private List<String> exposedApps = new ArrayList<>();

    /**
     * When {@code true} (default), Spring Boot autoconfigures
     * {@link VesperaProxyController} as a catch-all proxy.  Set to
     * {@code false} when you want to provide your own controller —
     * the {@link VesperaBridge} native methods remain available
     * for direct use either way.
     */
    private boolean controllerEnabled = true;

    /**
     * Who answers CORS for proxied requests: {@code rust} (default — CORS
     * preflights are routed to the Rust router, whose
     * {@code Access-Control-*} headers win) or {@code spring} (Spring MVC /
     * Spring Security answer preflights; Rust's duplicates are dropped). See
     * {@link CorsHandling}.
     */
    private CorsHandling cors = CorsHandling.RUST;

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
     * heap-buffered SYNC / ASYNC paths. Default 64 MiB keeps SmartDispatch's
     * small unsafe fast path bounded; set {@code 0} to restore unlimited
     * buffering. Forwarded to the native buffered-response cap (unless
     * {@code vespera.bridge.native.max-buffered-response-bytes} or the
     * {@code vespera.limits.maxBufferedResponseBytes} system property /
     * {@code VESPERA_MAX_BUFFERED_RESPONSE_BYTES} variable sets it), so Rust
     * rejects an oversized response with {@code 500} before buffering it.
     * Large/unknown downloads should use streaming modes.
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

    /**
     * Native library (Cargo crate) name the autoconfiguration initialises
     * with {@link VesperaBridge#init(String)} when the application has not
     * done so itself. {@code null} (default) leaves initialisation to the
     * application.
     */
    private String libraryName;

    /**
     * Whether closing the Spring context also shuts the native runtime down
     * ({@link VesperaBridge#shutdown}). Default {@code false}: the shutdown is
     * irreversible for the JVM (the native library cannot be reloaded), so it
     * only applies when the autoconfiguration itself initialised the library
     * via {@link #libraryName}.
     */
    private boolean shutdownOnClose = false;

    /** How long {@link #shutdownOnClose} waits for in-flight native tasks. */
    private Duration shutdownTimeout = Duration.ofSeconds(10);

    /** Request-id handling of the proxy controller. */
    private final RequestId requestId = new RequestId();

    /**
     * Path patterns (Spring {@code PathPattern} syntax, matched against the
     * path within the application) the proxy controller serves.  Default
     * {@code /**}: every path no more specific Spring MVC handler claims.
     * Paths outside the list are not mapped to the proxy and fall through to
     * the rest of Spring MVC (other handler mappings, static resources, then
     * {@code 404}).  Also limits which CORS preflights {@code cors=rust}
     * routes to Rust.
     */
    private List<String> includePatterns = new ArrayList<>(ProxyPathPatterns.DEFAULT_INCLUDE);

    /**
     * Path patterns (Spring {@code PathPattern} syntax) the proxy controller
     * never serves, even when they match {@link #includePatterns} (e.g.
     * {@code /static/**}).  Excluded requests fall through to the rest of
     * Spring MVC (other handler mappings, static resources, then
     * {@code 404}).  Default: none.
     */
    private List<String> excludePatterns = new ArrayList<>();

    public String getAppHeader() {
        return appHeader;
    }

    public void setAppHeader(String appHeader) {
        this.appHeader = appHeader;
    }

    public List<String> getExposedApps() {
        return exposedApps;
    }

    public void setExposedApps(List<String> exposedApps) {
        this.exposedApps = exposedApps == null ? new ArrayList<>() : new ArrayList<>(exposedApps);
    }

    public boolean isControllerEnabled() {
        return controllerEnabled;
    }

    public void setControllerEnabled(boolean controllerEnabled) {
        this.controllerEnabled = controllerEnabled;
    }

    public CorsHandling getCors() {
        return cors;
    }

    public void setCors(CorsHandling cors) {
        this.cors = cors == null ? CorsHandling.RUST : cors;
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

    public String getLibraryName() {
        return libraryName;
    }

    public void setLibraryName(String libraryName) {
        this.libraryName = libraryName;
    }

    public boolean isShutdownOnClose() {
        return shutdownOnClose;
    }

    public void setShutdownOnClose(boolean shutdownOnClose) {
        this.shutdownOnClose = shutdownOnClose;
    }

    public Duration getShutdownTimeout() {
        return shutdownTimeout;
    }

    public void setShutdownTimeout(Duration shutdownTimeout) {
        this.shutdownTimeout = shutdownTimeout == null ? Duration.ZERO : shutdownTimeout;
    }

    public RequestId getRequestId() {
        return requestId;
    }

    public List<String> getIncludePatterns() {
        return includePatterns;
    }

    public void setIncludePatterns(List<String> includePatterns) {
        this.includePatterns = includePatterns == null ? new ArrayList<>() : new ArrayList<>(includePatterns);
    }

    public List<String> getExcludePatterns() {
        return excludePatterns;
    }

    public void setExcludePatterns(List<String> excludePatterns) {
        this.excludePatterns = excludePatterns == null ? new ArrayList<>() : new ArrayList<>(excludePatterns);
    }

    /**
     * {@link #includePatterns} / {@link #excludePatterns}, validated (an empty
     * include list means {@code /**}).
     *
     * @throws IllegalArgumentException for an invalid pattern
     */
    ProxyPathPatterns proxyPathPatterns() {
        return new ProxyPathPatterns(includePatterns, excludePatterns);
    }

    /**
     * {@code vespera.bridge.request-id.*}: when enabled, the proxy uses the
     * client's request id (a 1–128 character visible-ASCII token) or
     * generates a UUID, forwards it to Rust under {@link #header}, echoes it
     * on the response unless Rust set that header, and exposes it as the
     * SLF4J MDC key {@code requestId} while dispatching.
     */
    public static class RequestId {
        /**
         * Whether the proxy reads (or generates), forwards, echoes and
         * MDC-logs a request id.  Default {@code true}.
         */
        private boolean enabled = true;

        /**
         * Request-id header: read from the client, forwarded to Rust and
         * echoed on the response.  Default {@code X-Request-Id}.
         */
        private String header = "X-Request-Id";

        public boolean isEnabled() {
            return enabled;
        }

        public void setEnabled(boolean enabled) {
            this.enabled = enabled;
        }

        public String getHeader() {
            return header;
        }

        public void setHeader(String header) {
            this.header = header;
        }

        /** The header the controller uses, or {@code null} when disabled. */
        String effectiveHeader() {
            return enabled && header != null && !header.isBlank() ? header.trim() : null;
        }
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
            apply(-1);
        }

        /**
         * As {@link #apply()}, also aligning the native buffered-response cap
         * with the bridge's {@code bridgeResponseCap}
         * ({@code vespera.bridge.max-buffered-response-bytes}, {@code < 0} =
         * don't) when no native value was configured explicitly (this
         * property, {@code -Dvespera.limits.maxBufferedResponseBytes},
         * {@code VESPERA_MAX_BUFFERED_RESPONSE_BYTES} or a programmatic
         * {@code configureLimits}): Rust then stops collecting an oversized
         * SYNC / ASYNC response before it is allocated on either heap.
         */
        void apply(long bridgeResponseCap) {
            if (maxBufferedResponseBytes == null && bridgeResponseCap >= 0
                    && !VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES.explicitlyConfigured()
                    && System.getenv("VESPERA_MAX_BUFFERED_RESPONSE_BYTES") == null) {
                VesperaBridge.configureLimits(-1, -1, bridgeResponseCap);
            }
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
