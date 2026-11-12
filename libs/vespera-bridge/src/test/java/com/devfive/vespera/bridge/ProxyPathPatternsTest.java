package com.devfive.vespera.bridge;

import static org.assertj.core.api.Assertions.assertThat;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.http.HttpServletRequest;
import java.util.Arrays;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.context.ApplicationContext;
import org.springframework.context.annotation.Configuration;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.servlet.HandlerExecutionChain;
import org.springframework.web.servlet.config.annotation.EnableWebMvc;
import org.springframework.web.servlet.mvc.method.RequestMappingInfo;
import org.springframework.web.servlet.mvc.method.annotation.RequestMappingHandlerMapping;
import org.springframework.web.util.ServletRequestPathUtils;

/**
 * {@code vespera.bridge.include-patterns} / {@code exclude-patterns}: the
 * proxy's request mapping is narrowed so excluded paths find no proxy handler
 * and fall through to the rest of Spring MVC.
 */
class ProxyPathPatternsTest {

    @Configuration(proxyBeanMethods = false)
    @EnableWebMvc
    static class Mvc {
    }

    @RestController
    static class OtherController {
        @GetMapping("/spring/hello")
        public String hello() {
            return "spring";
        }
    }

    private final WebApplicationContextRunner runner = new WebApplicationContextRunner()
            .withUserConfiguration(Mvc.class, OtherController.class)
            .withConfiguration(AutoConfigurations.of(VesperaBridgeAutoConfiguration.class));

    private static MockHttpServletRequest request(String method, String path) {
        MockHttpServletRequest request = new MockHttpServletRequest(method, path);
        ServletRequestPathUtils.parseAndCache(request);
        return request;
    }

    /** The handler {@code requestMappingHandlerMapping} picks, or {@code null}. */
    private static Object handlerFor(ApplicationContext ctx, HttpServletRequest request) throws Exception {
        RequestMappingHandlerMapping mapping = ctx.getBean(
                "requestMappingHandlerMapping", RequestMappingHandlerMapping.class);
        HandlerExecutionChain chain = mapping.getHandler(request);
        return chain == null ? null : chain.getHandler();
    }

    private static boolean proxied(ApplicationContext ctx, String path) throws Exception {
        Object handler = handlerFor(ctx, request("GET", path));
        return handler instanceof HandlerMethod method && VesperaProxyPathMappings.isProxyMethod(method);
    }

    private static List<RequestMappingInfo> proxyMappings(ApplicationContext ctx) {
        RequestMappingHandlerMapping mapping = ctx.getBean(
                "requestMappingHandlerMapping", RequestMappingHandlerMapping.class);
        return mapping.getHandlerMethods().entrySet().stream()
                .filter(e -> VesperaProxyPathMappings.isProxyMethod(e.getValue()))
                .map(Map.Entry::getKey)
                .toList();
    }

    @Test
    void defaultsKeepTheAnnotationCatchAll() {
        runner.run(ctx -> {
            ProxyPathPatterns patterns = ctx.getBean(VesperaProxyPathMappings.class).patterns();
            assertTrue(patterns.isDefault());
            List<RequestMappingInfo> infos = proxyMappings(ctx);
            assertEquals(1, infos.size());
            assertEquals(java.util.Set.of("/**"), infos.get(0).getPatternValues());
            assertNull(infos.get(0).getCustomCondition());
            assertTrue(proxied(ctx, "/users"));
            assertTrue(proxied(ctx, "/static/app.css"));
            assertFalse(proxied(ctx, "/spring/hello"), "more specific controllers still win");
        });
    }

    @Test
    void excludedPathsAreNotMappedToTheProxy() {
        runner.withPropertyValues(
                        "vespera.bridge.exclude-patterns=/static/**,/spring/**,/*.html")
                .run(ctx -> {
                    assertEquals(1, proxyMappings(ctx).size());
                    assertTrue(proxied(ctx, "/users"));
                    assertTrue(proxied(ctx, "/users/1/orders"));
                    assertFalse(proxied(ctx, "/static/app.css"));
                    assertNull(handlerFor(ctx, request("GET", "/static/app.css")),
                            "excluded path falls through to the next handler mapping");
                    assertNull(handlerFor(ctx, request("POST", "/index.html")));
                    assertFalse(proxied(ctx, "/spring/missing"));
                    Object other = handlerFor(ctx, request("GET", "/spring/hello"));
                    assertSame(OtherController.class,
                            assertInstanceOf(HandlerMethod.class, other).getBeanType());
                });
    }

    @Test
    void includePatternsNarrowTheProxy() {
        runner.withPropertyValues(
                        "vespera.bridge.include-patterns=/api/**,/health",
                        "vespera.bridge.exclude-patterns=/api/internal/**")
                .run(ctx -> {
                    assertEquals(java.util.Set.of("/api/**", "/health"),
                            proxyMappings(ctx).get(0).getPatternValues());
                    assertTrue(proxied(ctx, "/api/users"));
                    assertTrue(proxied(ctx, "/health"));
                    assertFalse(proxied(ctx, "/users"));
                    assertFalse(proxied(ctx, "/api/internal/metrics"));
                    assertNull(handlerFor(ctx, request("GET", "/users")));
                });
    }

    @Test
    void preflightMappingHonoursThePatterns() throws Exception {
        runner.withPropertyValues("vespera.bridge.exclude-patterns=/static/**").run(ctx -> {
            VesperaPreflightHandlerMapping mapping = ctx.getBean(VesperaPreflightHandlerMapping.class);
            assertNotNull(mapping.getHandler(preflight("/users")));
            assertNull(mapping.getHandler(preflight("/static/app.css")));
        });
        // without a cached parsed path (e.g. ant_path_matcher) the raw lookup path is used
        VesperaPreflightHandlerMapping plain = new VesperaPreflightHandlerMapping(
                new VesperaProxyController(r -> null, r -> DispatchMode.SYNC),
                new ProxyPathPatterns(List.of("/api/**"), List.of()));
        MockHttpServletRequest api = preflight("/api/users");
        api.removeAttribute(ServletRequestPathUtils.PATH_ATTRIBUTE);
        assertNotNull(plain.getHandler(api));
        MockHttpServletRequest other = preflight("/other");
        other.removeAttribute(ServletRequestPathUtils.PATH_ATTRIBUTE);
        assertNull(plain.getHandler(other));
    }

    private static MockHttpServletRequest preflight(String path) {
        MockHttpServletRequest request = request("OPTIONS", path);
        request.addHeader("Origin", "https://app.example");
        request.addHeader("Access-Control-Request-Method", "POST");
        return request;
    }

    @Test
    void invalidPatternsFailStartup() {
        runner.withPropertyValues("vespera.bridge.exclude-patterns=/{bad")
                .run(ctx -> assertThat(ctx).hasFailed()
                        .getFailure().hasStackTraceContaining("Invalid vespera.bridge.exclude-patterns entry"));
    }

    @Test
    void controllerDisabledHasNoPathMappings() {
        runner.withPropertyValues("vespera.bridge.controller-enabled=false",
                        "vespera.bridge.exclude-patterns=/static/**")
                .run(ctx -> assertThat(ctx).doesNotHaveBean(VesperaProxyPathMappings.class));
    }

    @Test
    void patternNormalisation() {
        ProxyPathPatterns blank = new ProxyPathPatterns(Arrays.asList(" ", null), null);
        assertTrue(blank.isDefault());
        assertEquals(List.of("/**"), blank.include());
        ProxyPathPatterns trimmed = new ProxyPathPatterns(List.of(" /api/** "), List.of(" /api/x "));
        assertEquals(List.of("/api/**"), trimmed.include());
        assertEquals(List.of("/api/x"), trimmed.exclude());
        assertFalse(trimmed.isDefault());
        assertTrue(trimmed.toString().contains("/api/x"));
        assertNull(ProxyPathPatterns.defaults().excludeCondition());
        assertNotNull(trimmed.excludeCondition());
        ProxyPathPatterns.ExcludeCondition condition = trimmed.excludeCondition();
        assertSame(condition, condition.combine(condition));
        assertEquals(0, condition.compareTo(condition, request("GET", "/")));
        assertTrue(condition.toString().contains("/api/x"));
        assertThrows(IllegalArgumentException.class,
                () -> new ProxyPathPatterns(List.of("/{bad"), List.of()));

        VesperaBridgeProperties props = new VesperaBridgeProperties();
        assertEquals(List.of("/**"), props.getIncludePatterns());
        assertTrue(props.getExcludePatterns().isEmpty());
        props.setIncludePatterns(null);
        props.setExcludePatterns(null);
        assertTrue(props.proxyPathPatterns().isDefault());
    }
}
