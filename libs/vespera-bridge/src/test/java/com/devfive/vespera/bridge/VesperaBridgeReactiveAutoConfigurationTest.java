package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.ReactiveWebApplicationContextRunner;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.context.annotation.Configuration;
import org.springframework.mock.http.server.reactive.MockServerHttpRequest;
import org.springframework.mock.web.server.MockServerWebExchange;
import org.springframework.test.web.reactive.server.WebTestClient;
import org.springframework.web.reactive.config.EnableWebFlux;
import reactor.core.publisher.Mono;

/**
 * {@link VesperaBridgeReactiveAutoConfiguration}: active only in a REACTIVE
 * web application (the servlet configuration only in a SERVLET one), every
 * bean replaceable, and the autoconfigured proxy reachable end to end.
 */
class VesperaBridgeReactiveAutoConfigurationTest {

    @Configuration(proxyBeanMethods = false)
    @EnableWebFlux
    static class WebFluxConfig {}

    private final ReactiveWebApplicationContextRunner reactive = new ReactiveWebApplicationContextRunner()
            .withConfiguration(AutoConfigurations.of(VesperaBridgeCoreAutoConfiguration.class,
                    VesperaBridgeAutoConfiguration.class, VesperaBridgeReactiveAutoConfiguration.class));

    @Test
    void reactiveApplicationGetsTheReactiveProxyOnly() {
        reactive.run(ctx -> {
            assertTrue(ctx.containsBean("vesperaReactiveProxyHandler"));
            assertEquals(1, ctx.getBeansOfType(VesperaReactiveHandlerMapping.class).size());
            assertEquals(1, ctx.getBeansOfType(VesperaReactivePreflightFilter.class).size());
            assertInstanceOf(ReactiveHeaderAppNameResolver.class, ctx.getBean(ReactiveAppNameResolver.class));
            assertInstanceOf(ExchangePrincipalResolver.class, ctx.getBean(ReactivePrincipalResolver.class));
            assertEquals(1, ctx.getBeansOfType(VesperaClient.class).size());
            assertEquals(1, ctx.getBeansOfType(VesperaBridgeNativeSettings.class).size());
            assertTrue(ctx.getBeansOfType(VesperaProxyController.class).isEmpty());
            assertTrue(ctx.getBeansOfType(AppNameResolver.class).isEmpty());
        });
    }

    @Test
    void servletApplicationGetsTheServletProxyOnly() {
        new WebApplicationContextRunner()
                .withConfiguration(AutoConfigurations.of(VesperaBridgeCoreAutoConfiguration.class,
                        VesperaBridgeAutoConfiguration.class, VesperaBridgeReactiveAutoConfiguration.class))
                .run(ctx -> {
                    assertEquals(1, ctx.getBeansOfType(VesperaProxyController.class).size());
                    assertTrue(ctx.getBeansOfType(VesperaReactiveProxyHandler.class).isEmpty());
                    assertTrue(ctx.getBeansOfType(ReactiveAppNameResolver.class).isEmpty());
                });
    }

    @Test
    void propertiesShapeTheReactiveBeans() {
        reactive.withPropertyValues(
                        "vespera.bridge.exposed-apps=public",
                        "vespera.bridge.forward-principal=false",
                        "vespera.bridge.cors=spring")
                .run(ctx -> {
                    ReactiveHeaderAppNameResolver apps =
                            (ReactiveHeaderAppNameResolver) ctx.getBean(ReactiveAppNameResolver.class);
                    assertTrue(apps.isRestricted());
                    MockServerWebExchange exchange = MockServerWebExchange.from(
                            MockServerHttpRequest.get("/x").header("X-Vespera-App", "admin"));
                    assertThrows(UnknownAppException.class, () -> apps.resolveAppName(exchange));
                    ReactivePrincipalResolver principals = ctx.getBean(ReactivePrincipalResolver.class);
                    assertFalse(principals instanceof ExchangePrincipalResolver);
                    assertTrue(ctx.getBeansOfType(VesperaReactivePreflightFilter.class).isEmpty());
                    assertEquals(1, ctx.getBeansOfType(VesperaReactiveHandlerMapping.class).size());
                });
        reactive.withPropertyValues("vespera.bridge.controller-enabled=false").run(ctx -> {
            assertTrue(ctx.getBeansOfType(VesperaReactiveProxyHandler.class).isEmpty());
            assertTrue(ctx.getBeansOfType(VesperaReactiveHandlerMapping.class).isEmpty());
            assertTrue(ctx.getBeansOfType(VesperaReactivePreflightFilter.class).isEmpty());
            assertEquals(1, ctx.getBeansOfType(VesperaClient.class).size());
        });
        reactive.withPropertyValues("vespera.bridge.include-patterns=/{bad").run(ctx ->
                assertTrue(ctx.getStartupFailure() != null, "invalid pattern fails startup"));
    }

    @Test
    void streamingExecutorIsBoundedConfigurableAndClosedWithTheContext() {
        VesperaStreamingExecutor[] seen = new VesperaStreamingExecutor[1];
        reactive.withPropertyValues(
                        "vespera.bridge.reactive-streaming.max-threads=3",
                        "vespera.bridge.reactive-streaming.max-queued=7")
                .run(ctx -> {
                    VesperaStreamingExecutor executor = ctx.getBean(VesperaStreamingExecutor.class);
                    assertEquals(3, executor.maxThreads());
                    assertEquals(7, executor.maxQueued());
                    assertFalse(executor.isClosed());
                    seen[0] = executor;
                });
        assertTrue(seen[0].isClosed(), "closed on context close");
        reactive.run(ctx -> {
            VesperaStreamingExecutor executor = ctx.getBean(VesperaStreamingExecutor.class);
            assertEquals(VesperaStreamingExecutor.defaultMaxThreads(), executor.maxThreads());
            assertEquals(VesperaStreamingExecutor.DEFAULT_MAX_QUEUED, executor.maxQueued());
        });
        reactive.withPropertyValues("vespera.bridge.controller-enabled=false").run(ctx ->
                assertTrue(ctx.getBeansOfType(VesperaStreamingExecutor.class).isEmpty()));
        VesperaStreamingExecutor mine = new VesperaStreamingExecutor(1, 0);
        reactive.withBean(VesperaStreamingExecutor.class, () -> mine).run(ctx ->
                assertSame(mine, ctx.getBean(VesperaStreamingExecutor.class)));
    }

    @Test
    void userBeansWin() {
        ReactiveAppNameResolver apps = exchange -> "custom";
        ReactivePrincipalResolver principals = exchange -> Mono.just(VesperaPrincipal.of("svc"));
        VesperaReactiveProxyHandler handler = new VesperaReactiveProxyHandler(apps);
        reactive.withBean(ReactiveAppNameResolver.class, () -> apps)
                .withBean(ReactivePrincipalResolver.class, () -> principals)
                .run(ctx -> {
                    assertSame(apps, ctx.getBean(ReactiveAppNameResolver.class));
                    assertSame(principals, ctx.getBean(ReactivePrincipalResolver.class));
                });
        reactive.withBean("myHandler", VesperaReactiveProxyHandler.class, () -> handler)
                .run(ctx -> {
                    assertSame(handler, ctx.getBean(VesperaReactiveProxyHandler.class));
                    assertEquals(1, ctx.getBeansOfType(VesperaReactiveHandlerMapping.class).size());
                });
    }

    @Test
    void autoconfiguredProxyServesTheUrlSpaceEndToEnd() {
        reactive.withUserConfiguration(WebFluxConfig.class)
                .withPropertyValues("vespera.bridge.exclude-patterns=/static/**")
                .run(ctx -> {
                    WebTestClient client = WebTestClient.bindToApplicationContext(ctx).build();
                    // No native library in the unit suite: the proxy answers 503 ...
                    ReactiveTestSupport.Result unavailable = ReactiveTestSupport.call(client.get().uri("/users/1"));
                    assertEquals(503, unavailable.status());
                    assertTrue(unavailable.header("X-Request-Id") != null);
                    assertEquals(ProxyErrors.NOT_INITIALIZED, unavailable.body());
                    // ... while excluded paths fall through to WebFlux (404).
                    assertEquals(404, ReactiveTestSupport.call(client.get().uri("/static/app.js")).status());
                });
    }

    @Test
    void metricsObserverIsWiredIntoTheReactiveProxy() {
        List<DispatchEvent> events = new java.util.concurrent.CopyOnWriteArrayList<>();
        reactive.withUserConfiguration(WebFluxConfig.class)
                .withBean(DispatchObserver.class, () -> events::add)
                .run(ctx -> {
                    assertEquals(503, ReactiveTestSupport.call(
                            WebTestClient.bindToApplicationContext(ctx).build().get().uri("/users")).status());
                    assertEquals(1, events.size());
                    assertEquals(503, events.get(0).status());
                });
    }
}
