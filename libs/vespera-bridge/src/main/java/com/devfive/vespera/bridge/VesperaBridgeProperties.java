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
}
