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
 *
 * <p>Users who want path-based, subdomain-based, or any other app
 * selection can register a custom {@code AppNameResolver} bean;
 * the autoconfigure module's {@code @ConditionalOnMissingBean}
 * gate automatically disables the default in that case.
 *
 * <p>Implementations must be safe to call from multiple servlet
 * threads concurrently.
 */
@FunctionalInterface
public interface AppNameResolver {

    /**
     * Resolve the app name for the supplied request.
     *
     * @return app name, or {@code null} / empty / whitespace to
     *         route to the default app
     */
    String resolveAppName(HttpServletRequest request);
}
