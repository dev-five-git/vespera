package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Strategy for picking which named app should receive an incoming
 * HTTP request.  Supports the multi-app routing surface exposed by
 * the Rust {@code register_app_named} API.
 *
 * <p>The autoconfigured default is {@link HeaderAppNameResolver} —
 * it reads the app name from the {@code X-Vespera-App} request
 * header (configurable via the {@code vespera.bridge.app-header}
 * property), falling back to the default app when the header is
 * absent.  This keeps Spring endpoints aligned with the URLs
 * published in vespera's {@code openapi.json} — there is no path
 * prefix that diverges from the Rust router's view of the world.
 * Because that header is client-controlled, restrict it with
 * {@code vespera.bridge.exposed-apps} (see {@link HeaderAppNameResolver}).
 *
 * <p>Users who want path-based, subdomain-based, or any other app
 * selection can register a custom {@code AppNameResolver} bean;
 * the autoconfigure module's {@code @ConditionalOnMissingBean}
 * gate automatically disables the default in that case.  A resolver
 * may throw {@link UnknownAppException} to reject a request with
 * {@code 404} before it is dispatched.
 *
 * <p>Implementations must be safe to call from multiple servlet
 * threads concurrently.
 */
@FunctionalInterface
public interface AppNameResolver {

    /** Name of the default app ({@code register_app} / {@code jni_app!}). */
    String DEFAULT_APP = "_default";

    /**
     * Resolve the app name for the supplied request.
     *
     * @return app name, or {@code null} / empty / whitespace to
     *         route to the default app
     * @throws UnknownAppException when the request selects an app
     *         that must not be reachable (answered with {@code 404})
     */
    String resolveAppName(HttpServletRequest request);

    /**
     * Lower-case name of the request header this resolver reads the app
     * selection from, or {@code null}.  The proxy does not forward that
     * header to Rust, so the routing hint never leaks into handlers.
     */
    default String selectionHeaderName() {
        return null;
    }
}
