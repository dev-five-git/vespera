package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.concurrent.CountDownLatch;
import java.util.concurrent.Executor;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.io.IOException;
import org.springframework.boot.web.servlet.FilterRegistrationBean;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.mock.web.MockFilterChain;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * Autoconfigure branch tests for the dispatch-mode policy beans.
 *
 * <p>The contract under test (0.2.0 default flip): the autoconfigured
 * default is {@link SmartDispatchModeResolver} (DIRECT/SYNC fast paths
 * for small bounded requests, measured 2.2–3.2 µs vs 24.1 µs);
 * {@code vespera.bridge.dispatch-mode=bidirectional-streaming} opts out
 * to {@link BidirectionalStreamingDispatchModeResolver} (pre-0.2.0
 * behavior); {@code vespera.bridge.dispatch-mode=smart} explicitly
 * pins the new default; a user-supplied bean always wins over all of
 * the above via {@code @ConditionalOnMissingBean}.
 */
class VesperaBridgeAutoConfigurationTest {

    // withConfiguration (not withUserConfiguration): autoconfigurations
    // must be evaluated AFTER user configs so @ConditionalOnMissingBean
    // sees user-supplied beans — same ordering as a real Boot app.
    private final WebApplicationContextRunner runner =
            new WebApplicationContextRunner()
                    .withConfiguration(AutoConfigurations.of(VesperaBridgeCoreAutoConfiguration.class, VesperaBridgeAutoConfiguration.class));

    @Test
    void exposedAppsPropertyRestrictsTheDefaultAppResolver() {
        runner.run(ctx -> assertFalse(
                ((HeaderAppNameResolver) ctx.getBean(AppNameResolver.class)).isRestricted()));
        runner.withPropertyValues("vespera.bridge.exposed-apps=public,reports")
                .run(ctx -> {
                    HeaderAppNameResolver resolver =
                            (HeaderAppNameResolver) ctx.getBean(AppNameResolver.class);
                    assertTrue(resolver.isRestricted());
                    MockHttpServletRequest req = new MockHttpServletRequest("GET", "/users");
                    req.addHeader("X-Vespera-App", "admin");
                    assertThrows(UnknownAppException.class, () -> resolver.resolveAppName(req));
                });
    }

    @Test
    void clientBeanIsExposedEvenWithoutTheControllerAndIsReplaceable() {
        runner.withPropertyValues("vespera.bridge.controller-enabled=false")
                .run(ctx -> {
                    VesperaClient client = ctx.getBean(VesperaClient.class);
                    assertEquals(null, client.appName());
                    assertFalse(ctx.containsBean("vesperaProxyController"));
                });
        VesperaClient custom = VesperaClient.create().forApp("admin");
        runner.withBean(VesperaClient.class, () -> custom)
                .run(ctx -> assertSame(custom, ctx.getBean(VesperaClient.class)));
    }

    @Test
    void defaultResolverIsSmart() {
        runner.run(
                ctx ->
                        assertInstanceOf(
                                SmartDispatchModeResolver.class,
                                ctx.getBean(DispatchModeResolver.class),
                                "0.2.0: autoconfigured default flipped to SmartDispatchModeResolver"));
    }

    @Test
    void smartPropertyExplicitlyPinsSmartResolver() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=smart")
                .run(
                        ctx ->
                                assertInstanceOf(
                                        SmartDispatchModeResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "explicit dispatch-mode=smart must keep the new default"));
    }

    @Test
    void bidirectionalStreamingPropertyOptsOutToStreamingResolver() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=bidirectional-streaming")
                .run(
                        ctx ->
                                assertInstanceOf(
                                        BidirectionalStreamingDispatchModeResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "dispatch-mode=bidirectional-streaming must restore the"
                                                + " pre-0.2.0 default"));
    }

    @Test
    void userBeanWinsOverDefault() {
        runner.withUserConfiguration(CustomResolverConfig.class)
                .run(
                        ctx ->
                                assertInstanceOf(
                                        CustomResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "@ConditionalOnMissingBean: user bean must win over the"
                                                + " autoconfigured smart default"));
    }

    @Test
    void userBeanWinsOverBidirectionalStreamingProperty() {
        runner.withPropertyValues("vespera.bridge.dispatch-mode=bidirectional-streaming")
                .withUserConfiguration(CustomResolverConfig.class)
                .run(
                        ctx ->
                                assertInstanceOf(
                                        CustomResolver.class,
                                        ctx.getBean(DispatchModeResolver.class),
                                        "@ConditionalOnMissingBean: user bean must win even when"
                                                + " the opt-out property is set"));
    }

    @Test
    void autoConfigurationBacksOffWithoutSpringMvc() {
        // Spring MVC is a provided dependency: an app without it (e.g. a
        // WebFlux or non-web app that only uses VesperaBridge directly) must
        // not get the servlet proxy wired in.
        runner.withClassLoader(new org.springframework.boot.test.context.FilteredClassLoader(
                        "org.springframework.web.servlet.DispatcherServlet"))
                .run(ctx -> {
                    assertTrue(ctx.getBeansOfType(VesperaProxyController.class).isEmpty());
                    assertTrue(ctx.getBeansOfType(DispatchModeResolver.class).isEmpty());
                });
    }

    @Test
    void autoConfigurationBacksOffOutsideServletWebApps() {
        new org.springframework.boot.test.context.runner.ApplicationContextRunner()
                .withConfiguration(AutoConfigurations.of(VesperaBridgeCoreAutoConfiguration.class, VesperaBridgeAutoConfiguration.class))
                .run(ctx -> assertTrue(ctx.getBeansOfType(VesperaProxyController.class).isEmpty()));
    }

    @Test
    void controllerDisabledPropertyStillWorks() {
        runner.withPropertyValues("vespera.bridge.controller-enabled=false")
                .run(ctx -> {
                    assertTrue(ctx.getBeansOfType(VesperaProxyController.class).isEmpty());
                    assertTrue(ctx.getBeansOfType(VesperaAsyncResponseBodyAdvice.class).isEmpty());
                });
    }

    @Test
    void asyncResponseBodyAdviceAccompaniesTheController() {
        runner.run(ctx -> assertEquals(1, ctx.getBeansOfType(VesperaAsyncResponseBodyAdvice.class).size()));
        // Also for a user-supplied controller.
        runner.withUserConfiguration(CustomControllerConfig.class)
                .run(ctx -> assertEquals(1, ctx.getBeansOfType(VesperaAsyncResponseBodyAdvice.class).size()));
    }

    @Test
    void directRetryOnOverflowDefaultsToTrueAndCanBeDisabled() {
        runner.run(ctx -> assertTrue(ctx.getBean(VesperaBridgeProperties.class).isDirectRetryOnOverflow()));
        runner.withPropertyValues("vespera.bridge.direct-retry-on-overflow=false")
                .run(ctx -> assertFalse(
                        ctx.getBean(VesperaBridgeProperties.class).isDirectRetryOnOverflow()));
    }

    @Test
    void perRequestThreadLocalCleanupFilterIsOptInAndClearsInFinally() {
        runner.run(ctx -> assertTrue(ctx.getBeansOfType(FilterRegistrationBean.class).isEmpty()));
        runner.withPropertyValues("vespera.bridge.clear-threadlocals-after-request=true")
                .run(ctx -> {
                    FilterRegistrationBean<?> bean = ctx.getBean(FilterRegistrationBean.class);
                    VesperaDirectBufferPool.directPoolForTest();
                    assertTrue(VesperaDirectBufferPool.directPoolPresentForTest());

                    bean.getFilter().doFilter(
                            new MockHttpServletRequest("GET", "/x"),
                            new MockHttpServletResponse(),
                            new MockFilterChain());

                    assertFalse(VesperaDirectBufferPool.directPoolPresentForTest());
                    assertTrue(ctx.getBean(VesperaBridgeProperties.class)
                            .isClearThreadlocalsAfterRequest());
                });
    }

    @Test
    void maxBufferedRequestBytesDefaultsToConservativeCapAndCanBeConfigured() {
        runner.run(ctx -> assertEquals(VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES,
                ctx.getBean(VesperaBridgeProperties.class).getMaxBufferedRequestBytes()));
        runner.withPropertyValues("vespera.bridge.max-buffered-request-bytes=12345")
                .run(ctx -> assertEquals(12345L,
                        ctx.getBean(VesperaBridgeProperties.class).getMaxBufferedRequestBytes()));
    }

    @Test
    void configuredAppHeaderFeedsDefaultResolver() {
        runner.withPropertyValues("vespera.bridge.app-header=X-Target-App")
                .run(ctx -> {
                    MockHttpServletRequest request = new MockHttpServletRequest("GET", "/x");
                    request.addHeader("X-Target-App", "admin");
                    assertEquals("admin", ctx.getBean(AppNameResolver.class).resolveAppName(request));
                });
    }

    @Test
    void userAppResolverWinsOverDefault() {
        runner.withUserConfiguration(CustomAppResolverConfig.class)
                .run(ctx -> assertSame(
                        CustomAppResolverConfig.RESOLVER,
                        ctx.getBean(AppNameResolver.class)));
    }

    @Test
    void userControllerWinsOverAutoconfiguredController() {
        runner.withUserConfiguration(CustomControllerConfig.class)
                .run(ctx -> assertSame(
                        CustomControllerConfig.CONTROLLER,
                        ctx.getBean(VesperaProxyController.class)));
    }

    @Test
    void userCleanupListenerWinsOverDefault() {
        runner.withUserConfiguration(CustomCleanupConfig.class)
                .run(ctx -> assertSame(
                        CustomCleanupConfig.CLEANUP,
                        ctx.getBean(VesperaBridgeThreadLocalCleanup.class)));
    }

    @Test
    void asyncResponseExecutorBeanIsReplaceableByName() {
        runner.withUserConfiguration(CustomExecutorConfig.class)
                .run(ctx -> assertSame(
                        CustomExecutorConfig.EXECUTOR,
                        ctx.getBean("vesperaBridgeAsyncResponseExecutor", Executor.class)));
    }

    @Test
    void defaultAsyncResponseExecutorUsesNamedDaemonThread() {
        runner.run(ctx -> {
            Executor executor = ctx.getBean("vesperaBridgeAsyncResponseExecutor", Executor.class);
            CountDownLatch done = new CountDownLatch(1);
            String[] name = {null};
            boolean[] daemon = {false};

            executor.execute(() -> {
                Thread current = Thread.currentThread();
                name[0] = current.getName();
                daemon[0] = current.isDaemon();
                done.countDown();
            });

            try {
                assertTrue(done.await(5, TimeUnit.SECONDS));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new AssertionError(e);
            }
            assertTrue(name[0].startsWith("vespera-bridge-async-response-"), name[0]);
            assertTrue(daemon[0]);
        });
    }

    @Test
    void defaultAsyncResponseExecutorUsesBoundedQueueWithAbortBackpressure() {
        // The executor's tasks are submitted from the thread that completes the
        // native dispatch future — a Rust Tokio worker. CallerRunsPolicy would
        // run the heavy wire-response build on that worker under saturation,
        // stealing native dispatch capacity; AbortPolicy rejects instead and the
        // proxy maps the rejection to a 503 (see VesperaProxyController
        // .asyncFailureToResponse). The bounded queue still absorbs bursts.
        runner.run(ctx -> {
            ThreadPoolExecutor executor = ctx.getBean(
                    "vesperaBridgeAsyncResponseExecutor", ThreadPoolExecutor.class);

            assertTrue(executor.getQueue().remainingCapacity() < Integer.MAX_VALUE);
            assertInstanceOf(ThreadPoolExecutor.AbortPolicy.class, executor.getRejectedExecutionHandler());
        });
    }

    @Test
    void configuredAsyncPoolSizeControlsBothExecutorBounds() {
        runner.withPropertyValues("vespera.bridge.async-pool-size=3")
                .run(ctx -> {
                    ThreadPoolExecutor executor = ctx.getBean(
                            "vesperaBridgeAsyncResponseExecutor", ThreadPoolExecutor.class);
                    assertEquals(3, executor.getCorePoolSize());
                    assertEquals(3, executor.getMaximumPoolSize());
                    assertEquals(768, executor.getQueue().remainingCapacity());
                });
    }

    @Test
    void nullDispatchModeStillUsesSmartDefault() {
        VesperaBridgeProperties properties = new VesperaBridgeProperties();
        properties.setDispatchMode(null);

        assertInstanceOf(SmartDispatchModeResolver.class,
                new VesperaBridgeAutoConfiguration()
                        .vesperaBridgeDispatchModeResolver(properties));
    }

    @Test
    void defaultResolverFactoryAcceptsBidirectionalValueWhenInvokedDirectly() {
        VesperaBridgeProperties properties = new VesperaBridgeProperties();
        properties.setDispatchMode("bidirectional-streaming");

        assertInstanceOf(SmartDispatchModeResolver.class,
                new VesperaBridgeAutoConfiguration()
                        .vesperaBridgeDispatchModeResolver(properties));
    }

    @Test
    void cleanupFilterClearsBuffersWhenDownstreamThrows() {
        runner.withPropertyValues("vespera.bridge.clear-threadlocals-after-request=true")
                .run(ctx -> {
                    FilterRegistrationBean<?> bean = ctx.getBean(FilterRegistrationBean.class);
                    VesperaDirectBufferPool.directPoolForTest();

                    assertThrows(IOException.class, () -> bean.getFilter().doFilter(
                            new MockHttpServletRequest("GET", "/x"),
                            new MockHttpServletResponse(),
                            (request, response) -> { throw new IOException("downstream"); }));

                    assertFalse(VesperaDirectBufferPool.directPoolPresentForTest());
                    assertEquals(Integer.MAX_VALUE, bean.getOrder());
                });
    }

    @Test
    void unknownDispatchModeFailsFast() {
        // A production typo must fail at bean creation instead of silently
        // enabling the smart DIRECT/SYNC policy.
        runner.withPropertyValues("vespera.bridge.dispatch-mode=not-a-real-mode")
                .run(ctx -> {
                    assertTrue(ctx.getStartupFailure() instanceof org.springframework.beans.factory.BeanCreationException);
                    assertTrue(ctx.getStartupFailure().getMessage().contains("not-a-real-mode"));
                    assertTrue(ctx.getStartupFailure().getMessage().contains("smart"));
                    assertTrue(ctx.getStartupFailure().getMessage().contains("bidirectional-streaming"));
                });
    }

    static final class CustomResolver implements DispatchModeResolver {
        @Override
        public DispatchMode resolveMode(jakarta.servlet.http.HttpServletRequest request) {
            return DispatchMode.SYNC;
        }
    }

    @Configuration(proxyBeanMethods = false)
    static class CustomResolverConfig {
        @Bean
        DispatchModeResolver customResolver() {
            return new CustomResolver();
        }
    }

    @Configuration(proxyBeanMethods = false)
    static class CustomExecutorConfig {
        static final Executor EXECUTOR = Runnable::run;

        @Bean("vesperaBridgeAsyncResponseExecutor")
        Executor vesperaBridgeAsyncResponseExecutor() {
            return EXECUTOR;
        }
    }

    @Configuration(proxyBeanMethods = false)
    static class CustomAppResolverConfig {
        static final AppNameResolver RESOLVER = request -> "custom";

        @Bean
        AppNameResolver customAppNameResolver() {
            return RESOLVER;
        }
    }

    @Test
    void nativePropertiesBindAndAreForwardedBeforeTheController() {
        try {
            runner.withPropertyValues(
                            "vespera.bridge.native.worker-threads=3",
                            "vespera.bridge.native.max-blocking-threads=64",
                            "vespera.bridge.native.max-request-bytes=1048576",
                            "vespera.bridge.native.max-streaming-request-bytes=0",
                            "vespera.bridge.native.max-buffered-response-bytes=2097152",
                            "vespera.bridge.native.dispatch-timeout=30s",
                            "vespera.bridge.native.async-max-in-flight=128",
                            "vespera.bridge.native.max-request-producers=32")
                    .run(ctx -> {
                        assertEquals(32L, pendingValue(VesperaNativeConfig.MAX_REQUEST_PRODUCERS),
                                "forwarded to the native knob");
                        assertTrue(ctx.containsBean("vesperaBridgeNativeSettings"));
                        VesperaBridgeProperties.Native n =
                                ctx.getBean(VesperaBridgeProperties.class).getNative();
                        assertEquals(3, n.getWorkerThreads());
                        assertEquals(64, n.getMaxBlockingThreads());
                        assertEquals(1048576L, n.getMaxRequestBytes());
                        assertEquals(0L, n.getMaxStreamingRequestBytes());
                        assertEquals(2097152L, n.getMaxBufferedResponseBytes());
                        assertEquals(30_000L, n.getDispatchTimeout().toMillis());
                        assertEquals(128, n.getAsyncMaxInFlight());
                        assertEquals(32, n.getMaxRequestProducers());
                    });
        } finally {
            VesperaNativeConfig.MAX_REQUEST_PRODUCERS.clearPendingForTest();
            VesperaNativeConfig.RUNTIME_WORKER_THREADS.clearPendingForTest();
            VesperaNativeConfig.RUNTIME_MAX_BLOCKING_THREADS.clearPendingForTest();
            VesperaNativeConfig.MAX_REQUEST_BYTES.clearPendingForTest();
            VesperaNativeConfig.MAX_STREAMING_REQUEST_BYTES.clearPendingForTest();
            VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES.clearPendingForTest();
            VesperaNativeConfig.DISPATCH_TIMEOUT_MILLIS.clearPendingForTest();
            VesperaNativeConfig.ASYNC_MAX_IN_FLIGHT.clearPendingForTest();
        }
    }

    private static Long pendingValue(VesperaNativeConfig.Knob knob) throws Exception {
        java.lang.reflect.Field pending = VesperaNativeConfig.Knob.class.getDeclaredField("pending");
        pending.setAccessible(true);
        return (Long) pending.get(knob);
    }

    @Test
    void unsetNativePropertiesStayNull() {
        runner.run(ctx -> {
            VesperaBridgeProperties.Native n =
                    ctx.getBean(VesperaBridgeProperties.class).getNative();
            assertEquals(null, n.getMaxRequestBytes());
            assertEquals(null, n.getDispatchTimeout());
        });
    }

    @Configuration(proxyBeanMethods = false)
    static class CustomControllerConfig {
        static final VesperaProxyController CONTROLLER = new VesperaProxyController(
                request -> null, request -> DispatchMode.SYNC);

        @Bean
        VesperaProxyController customController() {
            return CONTROLLER;
        }
    }

    @Configuration(proxyBeanMethods = false)
    static class CustomCleanupConfig {
        static final VesperaBridgeThreadLocalCleanup CLEANUP =
                new VesperaBridgeThreadLocalCleanup();

        @Bean
        VesperaBridgeThreadLocalCleanup customCleanup() {
            return CLEANUP;
        }
    }
}
