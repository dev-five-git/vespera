package com.devfive.vespera.bridge;

import org.springframework.core.Ordered;
import org.springframework.web.cors.reactive.CorsUtils;
import org.springframework.web.server.ServerWebExchange;
import org.springframework.web.server.WebFilter;
import org.springframework.web.server.WebFilterChain;
import reactor.core.publisher.Mono;

import java.util.Objects;

/**
 * Hands CORS preflight requests straight to {@link VesperaReactiveProxyHandler}
 * ({@code vespera.bridge.cors=rust}, the default), so the Rust router (e.g. a
 * {@code CorsLayer}) answers them.
 *
 * <p>WebFlux's {@code DispatcherHandler} never invokes a handler for a
 * preflight — it only lets the handler mappings apply Spring's own CORS
 * configuration — so a handler mapping cannot route preflights to Rust. This
 * filter runs late in the {@code WebFilter} chain ({@link #DEFAULT_ORDER},
 * after Spring Security's {@code WebFilterChainProxy}) and only for preflights
 * whose path the proxy owns ({@link ProxyPathPatterns}); every other request
 * continues down the chain.
 */
public final class VesperaReactivePreflightFilter implements WebFilter, Ordered {

    /** Default order: after security and the application's own filters. */
    public static final int DEFAULT_ORDER = Ordered.LOWEST_PRECEDENCE - 10;

    private final VesperaReactiveProxyHandler handler;
    private final ProxyPathPatterns patterns;

    public VesperaReactivePreflightFilter(VesperaReactiveProxyHandler handler, ProxyPathPatterns patterns) {
        this.handler = Objects.requireNonNull(handler, "handler");
        this.patterns = Objects.requireNonNull(patterns, "patterns");
    }

    @Override
    public Mono<Void> filter(ServerWebExchange exchange, WebFilterChain chain) {
        if (CorsUtils.isPreFlightRequest(exchange.getRequest())
                && patterns.matches(exchange.getRequest().getPath().pathWithinApplication())) {
            return handler.handle(exchange);
        }
        return chain.filter(exchange);
    }

    @Override
    public int getOrder() {
        return DEFAULT_ORDER;
    }
}
