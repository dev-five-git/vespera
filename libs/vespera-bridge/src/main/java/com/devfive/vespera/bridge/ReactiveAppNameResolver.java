package com.devfive.vespera.bridge;

import org.springframework.web.server.ServerWebExchange;

/**
 * WebFlux counterpart of {@link AppNameResolver}: picks which named Rust app
 * receives a request proxied by {@link VesperaReactiveProxyHandler}.
 *
 * <p>The autoconfigured default is {@link ReactiveHeaderAppNameResolver}
 * ({@code vespera.bridge.app-header}, default {@code X-Vespera-App},
 * restricted by {@code vespera.bridge.exposed-apps}). Register a custom
 * {@code ReactiveAppNameResolver} bean to replace it. A resolver may throw
 * {@link UnknownAppException} to reject a request with {@code 404} before it
 * is dispatched.
 *
 * <p>Called on the request's event-loop thread: implementations must not
 * block and must be safe to call concurrently.
 */
@FunctionalInterface
public interface ReactiveAppNameResolver {

    /**
     * Resolve the app name for the supplied exchange.
     *
     * @return app name, or {@code null} / empty / whitespace to route to the
     *         default app
     * @throws UnknownAppException when the request selects an app that must
     *         not be reachable (answered with {@code 404})
     */
    String resolveAppName(ServerWebExchange exchange);

    /**
     * Lower-case name of the request header this resolver reads the app
     * selection from, or {@code null}. The proxy does not forward that header
     * to Rust.
     */
    default String selectionHeaderName() {
        return null;
    }
}
