package com.devfive.vespera.bridge;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.beans.factory.ObjectProvider;
import org.springframework.beans.factory.SmartInitializingSingleton;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.servlet.mvc.method.RequestMappingInfo;
import org.springframework.web.servlet.mvc.method.annotation.RequestMappingHandlerMapping;

import java.lang.reflect.Method;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Objects;

/**
 * Applies {@code vespera.bridge.include-patterns} /
 * {@code vespera.bridge.exclude-patterns} to the proxy's request mapping.
 *
 * <p>{@link VesperaProxyController#proxy} is declared as
 * {@code @RequestMapping("/**")} and detected by Spring MVC's
 * {@link RequestMappingHandlerMapping}.  When the configured patterns differ
 * from that catch-all, this bean — once every singleton (including the
 * handler mappings) is initialised and before the web server accepts
 * requests — replaces that registration with one whose path patterns are the
 * include list and whose custom condition rejects excluded paths.  An excluded
 * (or not included) request therefore finds no proxy mapping, and
 * {@code DispatcherServlet} carries on with the remaining handler mappings:
 * other controllers, static resources, and finally {@code 404}.
 *
 * <p>With the defaults ({@code /**}, no exclusions) the annotation mapping is
 * left untouched.
 */
public final class VesperaProxyPathMappings implements SmartInitializingSingleton {

    private static final Logger log = LoggerFactory.getLogger(VesperaProxyPathMappings.class);

    private final ProxyPathPatterns patterns;
    private final ObjectProvider<RequestMappingHandlerMapping> handlerMappings;

    public VesperaProxyPathMappings(ProxyPathPatterns patterns,
                                    ObjectProvider<RequestMappingHandlerMapping> handlerMappings) {
        this.patterns = Objects.requireNonNull(patterns, "patterns");
        this.handlerMappings = Objects.requireNonNull(handlerMappings, "handlerMappings");
    }

    public ProxyPathPatterns patterns() {
        return patterns;
    }

    @Override
    public void afterSingletonsInstantiated() {
        if (patterns.isDefault()) {
            return;
        }
        int remapped = handlerMappings.orderedStream().mapToInt(this::remap).sum();
        if (remapped == 0) {
            log.warn("vespera.bridge include/exclude patterns ({}) were not applied: no "
                    + "VesperaProxyController#proxy request mapping was found", patterns);
        }
    }

    /** Re-register every proxy mapping of {@code mapping}; returns how many. */
    int remap(RequestMappingHandlerMapping mapping) {
        Map<RequestMappingInfo, HandlerMethod> proxies = new LinkedHashMap<>();
        mapping.getHandlerMethods().forEach((info, handler) -> {
            if (isProxyMethod(handler)) {
                proxies.put(info, handler);
            }
        });
        proxies.forEach((info, handler) -> {
            RequestMappingInfo.Builder builder = info.mutate()
                    .paths(patterns.include().toArray(String[]::new));
            ProxyPathPatterns.ExcludeCondition exclude = patterns.excludeCondition();
            if (exclude != null) {
                builder.customCondition(exclude);
            }
            mapping.unregisterMapping(info);
            mapping.registerMapping(builder.build(), handler.getBean(), handler.getMethod());
        });
        return proxies.size();
    }

    static boolean isProxyMethod(HandlerMethod handler) {
        Class<?> beanType = handler.getBeanType();
        Method method = handler.getMethod();
        Method proxy = VesperaPreflightHandlerMapping.proxyMethod();
        return VesperaProxyController.class.isAssignableFrom(beanType)
                && method.getName().equals(proxy.getName())
                && java.util.Arrays.equals(method.getParameterTypes(), proxy.getParameterTypes());
    }
}
