package com.devfive.vespera.bridge;

import org.springframework.core.Ordered;
import org.springframework.web.reactive.HandlerMapping;
import org.springframework.web.server.ServerWebExchange;
import reactor.core.publisher.Mono;

import java.util.Objects;

/**
 * Maps the URL space the proxy owns ({@code vespera.bridge.include-patterns}
 * minus {@code exclude-patterns}, see {@link ProxyPathPatterns}, matched
 * against the path within the application) to
 * {@link VesperaReactiveProxyHandler}.
 *
 * <p>Ordered {@link #DEFAULT_ORDER after} the annotated-controller
 * ({@code RequestMappingHandlerMapping}, order {@code 0}) and functional
 * ({@code RouterFunctionMapping}, order {@code -1}) mappings, so the
 * application's own WebFlux endpoints win over the catch-all, but before
 * static resources ({@code Ordered.LOWEST_PRECEDENCE - 1}) — the same
 * precedence the servlet proxy's {@code /**} mapping has. Paths outside the
 * proxied space fall through to the rest of WebFlux.
 */
public final class VesperaReactiveHandlerMapping implements HandlerMapping, Ordered {

    /** Default order: after controllers and router functions, before static resources. */
    public static final int DEFAULT_ORDER = Ordered.LOWEST_PRECEDENCE - 10;

    private final VesperaReactiveProxyHandler handler;
    private final ProxyPathPatterns patterns;
    private final int order;

    public VesperaReactiveHandlerMapping(VesperaReactiveProxyHandler handler, ProxyPathPatterns patterns) {
        this(handler, patterns, DEFAULT_ORDER);
    }

    public VesperaReactiveHandlerMapping(
            VesperaReactiveProxyHandler handler, ProxyPathPatterns patterns, int order) {
        this.handler = Objects.requireNonNull(handler, "handler");
        this.patterns = Objects.requireNonNull(patterns, "patterns");
        this.order = order;
    }

    @Override
    public Mono<Object> getHandler(ServerWebExchange exchange) {
        return patterns.matches(exchange.getRequest().getPath().pathWithinApplication())
                ? Mono.just(handler)
                : Mono.empty();
    }

    @Override
    public int getOrder() {
        return order;
    }
}
