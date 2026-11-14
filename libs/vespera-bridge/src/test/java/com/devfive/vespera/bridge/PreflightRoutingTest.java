package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.options;

import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.context.annotation.Configuration;
import org.springframework.http.HttpHeaders;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.mock.web.MockServletContext;
import org.springframework.test.web.servlet.MockMvc;
import org.springframework.test.web.servlet.setup.MockMvcBuilders;
import org.springframework.web.bind.annotation.CrossOrigin;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.context.annotation.AnnotationConfigUtils;
import org.springframework.web.context.support.GenericWebApplicationContext;
import org.springframework.web.servlet.config.annotation.EnableWebMvc;

/**
 * {@link VesperaPreflightHandlerMapping} in a real Spring MVC context: a
 * preflight whose actual request a Java controller serves is answered by
 * Spring (with that controller's {@code @CrossOrigin}); every other owned
 * preflight reaches the proxy (here: {@code 503}, no native library).
 */
class PreflightRoutingTest {

    @Configuration(proxyBeanMethods = false)
    @EnableWebMvc
    static class MvcConfig {}

    @RestController
    static class JavaController {
        @CrossOrigin(origins = "https://java.example")
        @GetMapping("/java/x")
        String x() {
            return "java";
        }
    }

    private final List<DispatchEvent> events = new CopyOnWriteArrayList<>();
    private GenericWebApplicationContext ctx;

    @AfterEach
    void close() {
        if (ctx != null) {
            ctx.close();
        }
    }

    private MockMvc mvc(ProxyPathPatterns patterns) {
        ctx = new GenericWebApplicationContext();
        AnnotationConfigUtils.registerAnnotationConfigProcessors(ctx);
        ctx.setServletContext(new MockServletContext());
        ctx.registerBean(MvcConfig.class);
        ctx.registerBean(JavaController.class);
        VesperaProxyController proxy = new VesperaProxyController(
                request -> AppNameResolver.DEFAULT_APP, request -> DispatchMode.SYNC,
                Runnable::run, true, 1024, 1024, null, List.of(events::add));
        ctx.registerBean(VesperaProxyController.class, () -> proxy);
        ctx.registerBean(VesperaPreflightHandlerMapping.class,
                () -> new VesperaPreflightHandlerMapping(proxy, patterns));
        ctx.refresh();
        return MockMvcBuilders.webAppContextSetup(ctx).build();
    }

    private static MockHttpServletResponse preflight(MockMvc mvc, String path, String origin, String method)
            throws Exception {
        return mvc.perform(options(path)
                        .header(HttpHeaders.ORIGIN, origin)
                        .header(HttpHeaders.ACCESS_CONTROL_REQUEST_METHOD, method))
                .andReturn().getResponse();
    }

    @Test
    void preflightForAJavaEndpointIsAnsweredBySpring() throws Exception {
        MockMvc mvc = mvc(ProxyPathPatterns.defaults());
        MockHttpServletResponse allowed = preflight(mvc, "/java/x", "https://java.example", "GET");
        assertEquals(200, allowed.getStatus());
        assertEquals("https://java.example", allowed.getHeader(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
        MockHttpServletResponse foreign = preflight(mvc, "/java/x", "https://evil.example", "GET");
        assertEquals(403, foreign.getStatus(), "Spring applies the @CrossOrigin policy");
        assertTrue(events.isEmpty(), "the proxy never ran");
    }

    @Test
    void preflightForARustPathStillReachesTheProxy() throws Exception {
        MockMvc mvc = mvc(ProxyPathPatterns.defaults());
        MockHttpServletResponse rust = preflight(mvc, "/users", "https://a.example", "PUT");
        assertEquals(503, rust.getStatus(), "proxied (no native library in this test)");
        assertNull(rust.getHeader(HttpHeaders.ACCESS_CONTROL_ALLOW_ORIGIN));
        assertEquals(1, events.size());
        assertEquals("OPTIONS", events.get(0).method());

        // Same path as the Java endpoint, but a method only the proxy serves:
        // the actual POST goes to Rust, so its preflight does too.
        assertEquals(503, preflight(mvc, "/java/x", "https://a.example", "POST").getStatus());
        assertEquals(2, events.size());
    }

    @Test
    void preflightOutsideTheProxiedSpaceIsLeftToSpring() throws Exception {
        MockMvc mvc = mvc(new ProxyPathPatterns(List.of("/api/**"), List.of()));
        preflight(mvc, "/other", "https://a.example", "GET");
        assertTrue(events.isEmpty());
    }
}
