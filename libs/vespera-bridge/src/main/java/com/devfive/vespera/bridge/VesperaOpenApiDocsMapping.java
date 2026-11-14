package com.devfive.vespera.bridge;

import java.lang.reflect.Method;
import java.util.Map;
import java.util.Objects;
import org.springframework.beans.factory.ListableBeanFactory;
import org.springframework.beans.factory.ObjectProvider;
import org.springframework.beans.factory.SmartInitializingSingleton;
import org.springframework.http.MediaType;
import org.springframework.web.bind.annotation.RequestMethod;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.servlet.mvc.method.RequestMappingInfo;
import org.springframework.web.servlet.mvc.method.annotation.RequestMappingHandlerMapping;

/**
 * Registers {@link VesperaOpenApiDocsController#openApi} at
 * {@code <docs-path>/{app}} on Spring MVC's
 * {@link RequestMappingHandlerMapping}, with the base path from
 * {@link VesperaSpringdocProperties#validatedDocsPath()} — the same
 * normalised value {@link VesperaSwaggerUiUrls} advertises, so a trailing
 * slash or a missing leading one cannot split the two, and a root / pattern
 * value fails the startup with a clear message instead of mapping
 * {@code /{app}} over the proxy.
 *
 * <p>Runs once every singleton (including the handler mappings) is
 * initialised, before the web server accepts requests. A controller whose
 * {@code openApi} method is already mapped (an application subclass with its
 * own {@code @GetMapping}) is left alone.
 */
final class VesperaOpenApiDocsMapping implements SmartInitializingSingleton {

    private static final String DEFAULT_MAPPING_BEAN = "requestMappingHandlerMapping";

    private final ListableBeanFactory beans;
    private final ObjectProvider<VesperaOpenApiDocsController> controller;
    private final String basePath;

    VesperaOpenApiDocsMapping(ListableBeanFactory beans,
                              ObjectProvider<VesperaOpenApiDocsController> controller,
                              VesperaSpringdocProperties props) {
        this.beans = Objects.requireNonNull(beans, "beans");
        this.controller = Objects.requireNonNull(controller, "controller");
        // Validate eagerly: a bad docs-path fails bean creation (startup).
        this.basePath = props.validatedDocsPath();
    }

    String basePath() {
        return basePath;
    }

    static Method openApiMethod() {
        try {
            return VesperaOpenApiDocsController.class.getMethod("openApi", String.class);
        } catch (NoSuchMethodException impossible) {
            throw new IllegalStateException(impossible);
        }
    }

    @Override
    public void afterSingletonsInstantiated() {
        VesperaOpenApiDocsController handler = controller.getIfUnique();
        RequestMappingHandlerMapping mapping = handlerMapping();
        if (handler == null || mapping == null || alreadyMapped(mapping)) {
            return;
        }
        RequestMappingInfo info = RequestMappingInfo.paths(basePath + "/{app}")
                .methods(RequestMethod.GET)
                .produces(MediaType.APPLICATION_JSON_VALUE)
                .options(mapping.getBuilderConfiguration())
                .build();
        mapping.registerMapping(info, handler, openApiMethod());
    }

    private RequestMappingHandlerMapping handlerMapping() {
        Map<String, RequestMappingHandlerMapping> all =
                beans.getBeansOfType(RequestMappingHandlerMapping.class, false, false);
        RequestMappingHandlerMapping primary = all.get(DEFAULT_MAPPING_BEAN);
        if (primary != null) {
            return primary;
        }
        return all.size() == 1 ? all.values().iterator().next() : null;
    }

    private static boolean alreadyMapped(RequestMappingHandlerMapping mapping) {
        for (HandlerMethod handler : mapping.getHandlerMethods().values()) {
            if (VesperaOpenApiDocsController.class.isAssignableFrom(handler.getBeanType())
                    && "openApi".equals(handler.getMethod().getName())) {
                return true;
            }
        }
        return false;
    }
}
