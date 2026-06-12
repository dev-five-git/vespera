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
     *   <li>{@code smart} (default since 1.0.0) — small bounded
     *       idempotent requests (Content-Length known and &le; 256
     *       KiB; GET/HEAD/PUT/DELETE/OPTIONS) take the pooled
     *       direct-buffer path, skipping JNI array copies and
     *       per-request stream setup; small non-idempotent requests
     *       (POST/PATCH) take heap-buffered SYNC; everything else
     *       falls back to bidirectional streaming.  Measured 2.2 µs
     *       (DIRECT) / 3.2 µs (SYNC) vs 24.1 µs (bidirectional) on
     *       a small {@code GET /health} round-trip.  Trade-offs:
     *       DIRECT re-runs the handler when a response overflows the
     *       pooled buffer ({@code vespera.direct.maxBufferBytes},
     *       default 4 MiB) — acceptable for idempotent requests
     *       only; SYNC fully buffers the response on the JVM heap.</li>
     *   <li>{@code bidirectional-streaming} — opt-out, restores the
     *       pre-1.0.0 default: every request that may carry a body
     *       streams both ways; safe for any payload size; the
     *       uniform per-request cost is ~24 µs even on small
     *       JSON-RPC payloads.</li>
     * </ul>
     */
    private String dispatchMode = "smart";

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
}
