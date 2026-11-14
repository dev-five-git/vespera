package com.devfive.vespera.bridge;

import org.springframework.beans.factory.BeanFactoryUtils;
import org.springframework.context.ApplicationContext;
import org.springframework.core.Ordered;
import org.springframework.core.annotation.AnnotationAwareOrderComparator;
import org.springframework.web.cors.CorsConfiguration;
import org.springframework.web.reactive.config.CorsRegistry;
import org.springframework.web.reactive.config.WebFluxConfigurer;
import org.springframework.web.reactive.handler.AbstractHandlerMapping;
import org.springframework.web.server.ServerWebExchange;
import reactor.core.publisher.Mono;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
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
 *
 * <p>An {@link AbstractHandlerMapping}, so Spring's CORS processing applies
 * to the proxied space. With {@code vespera.bridge.cors=spring} the global
 * CORS configuration of the context's {@link WebFluxConfigurer} beans
 * ({@code addCorsMappings}) is picked up when the mapping is a context bean
 * (the autoconfigured one is) — actual requests are checked and answered
 * with Spring's {@code Access-Control-*} headers, and preflights are
 * answered by Spring (a rejected origin gets {@code 403}) instead of an
 * empty {@code 200}. Like any handler mapping it can also be given a
 * configuration explicitly ({@link #setCorsConfigurations} /
 * {@link #setCorsConfigurationSource}). With {@code cors=rust} no Spring
 * CORS configuration is applied: Rust owns CORS
 * ({@link VesperaReactivePreflightFilter} routes preflights to it).
 */
public final class VesperaReactiveHandlerMapping extends AbstractHandlerMapping {

    /** Default order: after controllers and router functions, before static resources. */
    public static final int DEFAULT_ORDER = Ordered.LOWEST_PRECEDENCE - 10;

    private final VesperaReactiveProxyHandler handler;
    private final ProxyPathPatterns patterns;
    private boolean corsConfiguredExplicitly;

    public VesperaReactiveHandlerMapping(VesperaReactiveProxyHandler handler, ProxyPathPatterns patterns) {
        this(handler, patterns, DEFAULT_ORDER);
    }

    public VesperaReactiveHandlerMapping(
            VesperaReactiveProxyHandler handler, ProxyPathPatterns patterns, int order) {
        this.handler = Objects.requireNonNull(handler, "handler");
        this.patterns = Objects.requireNonNull(patterns, "patterns");
        setOrder(order);
    }

    @Override
    protected Mono<?> getHandlerInternal(ServerWebExchange exchange) {
        return patterns.matches(exchange.getRequest().getPath().pathWithinApplication())
                ? Mono.just(handler)
                : Mono.empty();
    }

    @Override
    public void setCorsConfigurations(Map<String, CorsConfiguration> corsConfigurations) {
        corsConfiguredExplicitly = true;
        super.setCorsConfigurations(corsConfigurations);
    }

    @Override
    public void setCorsConfigurationSource(
            org.springframework.web.cors.reactive.CorsConfigurationSource corsConfigurationSource) {
        corsConfiguredExplicitly = true;
        super.setCorsConfigurationSource(corsConfigurationSource);
    }

    /** With {@code cors=spring}: adopt the global CORS mappings of the context's {@link WebFluxConfigurer}s. */
    @Override
    protected void initApplicationContext(ApplicationContext context) {
        super.initApplicationContext(context);
        if (handler.cors() != CorsHandling.SPRING || corsConfiguredExplicitly) {
            return;
        }
        Map<String, CorsConfiguration> global = globalCorsConfigurations(context);
        if (!global.isEmpty()) {
            super.setCorsConfigurations(global);
        }
    }

    /**
     * The CORS mappings the context's {@link WebFluxConfigurer} beans register
     * in {@code addCorsMappings}, in configurer order — what Spring's
     * {@code DelegatingWebFluxConfiguration} gives its own handler mappings.
     */
    static Map<String, CorsConfiguration> globalCorsConfigurations(ApplicationContext context) {
        List<WebFluxConfigurer> configurers = new ArrayList<>(BeanFactoryUtils
                .beansOfTypeIncludingAncestors(context, WebFluxConfigurer.class).values());
        AnnotationAwareOrderComparator.sort(configurers);
        CollectingCorsRegistry registry = new CollectingCorsRegistry();
        for (WebFluxConfigurer configurer : configurers) {
            configurer.addCorsMappings(registry);
        }
        return registry.configurations();
    }

    /** Exposes the {@code protected} result of a {@link CorsRegistry}. */
    private static final class CollectingCorsRegistry extends CorsRegistry {
        Map<String, CorsConfiguration> configurations() {
            return getCorsConfigurations();
        }
    }
}
