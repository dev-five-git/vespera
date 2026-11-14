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
import org.springframework.http.server.reactive.ServerHttpResponse;
import org.springframework.http.server.reactive.ServerHttpResponseDecorator;
import org.springframework.web.cors.reactive.CorsUtils;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.reactive.HandlerMapping;
import org.springframework.web.reactive.result.method.RequestMappingInfo;
import org.springframework.web.reactive.result.method.RequestMappingInfoHandlerMapping;
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
 * would get; annotated controllers are first matched against the preflight
 * itself with Spring's preflight rules, which skip {@code consumes} /
 * {@code produces} / {@code headers} conditions. When that is not the Vespera proxy (a {@code @RestController}
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
                .concatMap(mapping -> {
                    if (mapping instanceof RequestMappingInfoHandlerMapping infoMapping) {
                        HandlerMethod best = bestPreflightMatch(infoMapping, privateView(exchange));
                        if (best != null) {
                            return Mono.just(best);
                        }
                    }
                    // Other mappings, and a controller mapping without a
                    // preflight match (its 405 / 415 … still counts).
                    return mapping.getHandler(actual);
                })
                .next()
                .map(found -> found instanceof VesperaReactiveProxyHandler)
                // No mapping at all: the proxy owns the path per its patterns.
                .defaultIfEmpty(Boolean.TRUE)
                // A mapping claims the path but rejects the request (405 …).
                .onErrorReturn(Boolean.FALSE)
                .flatMap(rust -> rust ? handler.handle(exchange) : chain.filter(exchange));
    }

    /**
     * The handler method an annotated-controller mapping would pick for the
     * real {@code preflight}, with Spring's own preflight matching (the
     * method condition matches {@code Access-Control-Request-Method};
     * {@code consumes} / {@code produces} / {@code headers} conditions are
     * skipped, since a preflight carries none of the actual request's
     * headers), or {@code null}. Asking the mapping with the actual request
     * instead would miss {@code @PostMapping(headers = "X-Api=1")} and leave
     * the preflight to the proxy.
     */
    static HandlerMethod bestPreflightMatch(RequestMappingInfoHandlerMapping mapping, ServerWebExchange preflight) {
        RequestMappingInfo bestInfo = null;
        HandlerMethod best = null;
        for (Map.Entry<RequestMappingInfo, HandlerMethod> entry : mapping.getHandlerMethods().entrySet()) {
            RequestMappingInfo match = entry.getKey().getMatchingCondition(preflight);
            if (match != null && (bestInfo == null || match.compareTo(bestInfo, preflight) < 0)) {
                bestInfo = match;
                best = entry.getValue();
            }
        }
        return best;
    }

    /**
     * The preflight itself with private attributes and private response
     * headers (a mapping's CORS processor writes {@code Vary} even for a
     * non-CORS request; it must not reach the real response).
     */
    static ServerWebExchange privateView(ServerWebExchange preflight) {
        HttpHeaders responseHeaders = new HttpHeaders();
        ServerHttpResponse response = new ServerHttpResponseDecorator(preflight.getResponse()) {
            @Override
            public HttpHeaders getHeaders() {
                return responseHeaders;
            }
        };
        Map<String, Object> attributes = new ConcurrentHashMap<>(preflight.getAttributes());
        return new ServerWebExchangeDecorator(preflight) {
            @Override
            public ServerHttpResponse getResponse() {
                return response;
            }

            @Override
            public Map<String, Object> getAttributes() {
                return attributes;
            }
        };
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
        return new ServerWebExchangeDecorator(privateView(preflight)) {
            @Override
            public ServerHttpRequest getRequest() {
                return request;
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
