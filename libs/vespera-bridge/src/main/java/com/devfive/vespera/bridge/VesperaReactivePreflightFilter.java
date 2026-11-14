package com.devfive.vespera.bridge;

import org.springframework.beans.factory.BeanFactoryUtils;
import org.springframework.context.ApplicationContext;
import org.springframework.context.ApplicationContextAware;
import org.springframework.core.Ordered;
import org.springframework.core.annotation.AnnotationAwareOrderComparator;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpMethod;
import org.springframework.http.server.reactive.ServerHttpRequest;
import org.springframework.http.server.reactive.ServerHttpRequestDecorator;
import org.springframework.web.cors.reactive.CorsUtils;
import org.springframework.web.reactive.HandlerMapping;
import org.springframework.web.server.ServerWebExchange;
import org.springframework.web.server.ServerWebExchangeDecorator;
import org.springframework.web.server.WebFilter;
import org.springframework.web.server.WebFilterChain;
import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.ConcurrentHashMap;

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
 *
 * <p>Inside the proxied space a preflight goes to Rust only when Rust would
 * also serve the actual request: as a bean (which receives the
 * {@link ApplicationContext}) it asks the application's
 * {@link HandlerMapping}s, in order, which handler the actual request
 * ({@code Access-Control-Request-Method}, same path, without {@code Origin})
 * would get. When that is not the Vespera proxy (a {@code @RestController}
 * with {@code @CrossOrigin}, a router function, an actuator endpoint …), or a
 * mapping rejects it (e.g. {@code 405}), the preflight continues down the
 * chain and Spring answers it with that handler's CORS configuration. The
 * lookup runs on a copy of the exchange attributes, so it leaves no trace.
 * Constructed outside a context it routes every owned preflight to Rust.
 */
public final class VesperaReactivePreflightFilter implements WebFilter, Ordered, ApplicationContextAware {

    /** Default order: after security and the application's own filters. */
    public static final int DEFAULT_ORDER = Ordered.LOWEST_PRECEDENCE - 10;

    private final VesperaReactiveProxyHandler handler;
    private final ProxyPathPatterns patterns;
    private ApplicationContext context;
    private volatile List<HandlerMapping> mappings;

    public VesperaReactivePreflightFilter(VesperaReactiveProxyHandler handler, ProxyPathPatterns patterns) {
        this.handler = Objects.requireNonNull(handler, "handler");
        this.patterns = Objects.requireNonNull(patterns, "patterns");
    }

    @Override
    public void setApplicationContext(ApplicationContext applicationContext) {
        this.context = applicationContext;
    }

    @Override
    public Mono<Void> filter(ServerWebExchange exchange, WebFilterChain chain) {
        if (!CorsUtils.isPreFlightRequest(exchange.getRequest())
                || !patterns.matches(exchange.getRequest().getPath().pathWithinApplication())) {
            return chain.filter(exchange);
        }
        List<HandlerMapping> all = handlerMappings();
        if (all.isEmpty()) {
            return handler.handle(exchange);
        }
        ServerWebExchange actual = actualRequest(exchange);
        return Flux.fromIterable(all)
                .concatMap(mapping -> mapping.getHandler(actual))
                .next()
                .map(found -> found instanceof VesperaReactiveProxyHandler)
                // No mapping at all: the proxy owns the path per its patterns.
                .defaultIfEmpty(Boolean.TRUE)
                // A mapping claims the path but rejects the request (405 …).
                .onErrorReturn(Boolean.FALSE)
                .flatMap(rust -> rust ? handler.handle(exchange) : chain.filter(exchange));
    }

    /**
     * The actual request the preflight announces: its
     * {@code Access-Control-Request-Method}, no CORS headers (so no mapping
     * applies CORS processing to the real response), private attributes.
     */
    static ServerWebExchange actualRequest(ServerWebExchange preflight) {
        ServerHttpRequest original = preflight.getRequest();
        HttpMethod method = HttpMethod.valueOf(
                original.getHeaders().getFirst(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD).trim());
        // A copy: a request builder may share (and so mutate) the original headers.
        HttpHeaders headers = new HttpHeaders();
        original.getHeaders().forEach((name, values) -> {
            if (!HttpHeaders.ORIGIN.equalsIgnoreCase(name)
                    && !HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD.equalsIgnoreCase(name)
                    && !HttpHeaders.ACCESS_CONTROL_REQUEST_HEADERS.equalsIgnoreCase(name)) {
                values.forEach(value -> headers.add(name, value));
            }
        });
        ServerHttpRequest request = new ServerHttpRequestDecorator(original) {
            @Override
            public HttpMethod getMethod() {
                return method;
            }

            @Override
            public HttpHeaders getHeaders() {
                return headers;
            }
        };
        Map<String, Object> attributes = new ConcurrentHashMap<>(preflight.getAttributes());
        return new ServerWebExchangeDecorator(preflight) {
            @Override
            public ServerHttpRequest getRequest() {
                return request;
            }

            @Override
            public Map<String, Object> getAttributes() {
                return attributes;
            }
        };
    }

    private List<HandlerMapping> handlerMappings() {
        List<HandlerMapping> resolved = mappings;
        if (resolved == null) {
            ApplicationContext ctx = context;
            if (ctx == null) {
                return List.of();
            }
            List<HandlerMapping> found = new ArrayList<>(BeanFactoryUtils.beansOfTypeIncludingAncestors(
                    ctx, HandlerMapping.class, true, false).values());
            AnnotationAwareOrderComparator.sort(found);
            resolved = List.copyOf(found);
            mappings = resolved;
        }
        return resolved;
    }

    @Override
    public int getOrder() {
        return DEFAULT_ORDER;
    }
}
