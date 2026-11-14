package com.devfive.vespera.bridge;

import static org.assertj.core.api.Assertions.assertThat;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.micrometer.core.instrument.MeterRegistry;
import io.micrometer.core.instrument.Timer;
import io.micrometer.core.instrument.simple.SimpleMeterRegistry;
import java.lang.reflect.Field;
import java.time.Duration;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.springframework.beans.factory.DisposableBean;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * Autoconfiguration of the observability / lifecycle features: the
 * Micrometer observer, request-id wiring, {@code library-name} auto-init and
 * {@code shutdown-on-close}.
 */
class ObservabilityAutoConfigurationTest {

    private final WebApplicationContextRunner runner = new WebApplicationContextRunner()
            .withConfiguration(AutoConfigurations.of(VesperaBridgeCoreAutoConfiguration.class,
                    VesperaBridgeAutoConfiguration.class,
                    VesperaBridgeMetricsAutoConfiguration.class));

    @Configuration(proxyBeanMethods = false)
    static class RegistryConfig {
        @Bean
        MeterRegistry meterRegistry() {
            return new SimpleMeterRegistry();
        }
    }

    @Test
    void micrometerObserverNeedsAMeterRegistryBean() {
        runner.run(ctx -> assertThat(ctx).doesNotHaveBean(MicrometerDispatchObserver.class));
        runner.withUserConfiguration(RegistryConfig.class)
                .run(ctx -> assertThat(ctx).hasSingleBean(MicrometerDispatchObserver.class));
    }

    @Test
    void controllerReportsToTheMicrometerTimerAndEchoesTheRequestId() {
        runner.withUserConfiguration(RegistryConfig.class).run(ctx -> {
            VesperaProxyController controller = ctx.getBean(VesperaProxyController.class);
            MockHttpServletRequest request = new MockHttpServletRequest("GET", "/x");
            // An unknown app is answered by the proxy itself (no native call).
            request.addHeader("X-Vespera-App", "nope");
            controller = new VesperaProxyController(
                    new HeaderAppNameResolver("X-Vespera-App", List.of("public")),
                    ctx.getBean(DispatchModeResolver.class), Runnable::run, true, 0, 0,
                    ctx.getBean(VesperaBridgeProperties.class).getRequestId().effectiveHeader(),
                    List.of(ctx.getBean(MicrometerDispatchObserver.class)));
            MockHttpServletResponse response = new MockHttpServletResponse();
            controller.proxy(request, response);

            assertEquals(404, response.getStatus());
            assertTrue(response.containsHeader("X-Request-Id"));
            Timer timer = ctx.getBean(MeterRegistry.class).get(MicrometerDispatchObserver.METER_NAME)
                    // The client-selected app is never a tag value verbatim.
                    .tags("app", "UNKNOWN", "mode", "NONE", "status", "404",
                            "outcome", "CLIENT_ERROR")
                    .timer();
            assertEquals(1, timer.count());
        });
    }

    @Test
    void requestIdPropertiesConfigureTheHeader() {
        runner.run(ctx -> assertEquals("X-Request-Id",
                ctx.getBean(VesperaBridgeProperties.class).getRequestId().effectiveHeader()));
        runner.withPropertyValues("vespera.bridge.request-id.header=X-Correlation-Id")
                .run(ctx -> assertEquals("X-Correlation-Id",
                        ctx.getBean(VesperaBridgeProperties.class).getRequestId().effectiveHeader()));
        runner.withPropertyValues("vespera.bridge.request-id.enabled=false")
                .run(ctx -> assertNull(
                        ctx.getBean(VesperaBridgeProperties.class).getRequestId().effectiveHeader()));
        VesperaBridgeProperties.RequestId blank = new VesperaBridgeProperties.RequestId();
        blank.setHeader(" ");
        assertNull(blank.effectiveHeader());
        blank.setHeader(null);
        assertNull(blank.effectiveHeader());
    }

    @Test
    void outcomeTagCoversEveryStatusClass() {
        assertEquals("PANIC", MicrometerDispatchObserver.outcome(500, true));
        assertEquals("SERVER_ERROR", MicrometerDispatchObserver.outcome(503, false));
        assertEquals("CLIENT_ERROR", MicrometerDispatchObserver.outcome(404, false));
        assertEquals("REDIRECTION", MicrometerDispatchObserver.outcome(302, false));
        assertEquals("SUCCESS", MicrometerDispatchObserver.outcome(200, false));
        assertEquals("INFORMATIONAL", MicrometerDispatchObserver.outcome(101, false));

        SimpleMeterRegistry registry = new SimpleMeterRegistry();
        new MicrometerDispatchObserver(registry).onComplete(new DispatchEvent(
                "admin", DispatchMode.DIRECT, "GET", "/p", 200, 1_000_000, false, "r"));
        assertEquals(1, registry.get("vespera.bridge.dispatch")
                .tags("app", "admin", "mode", "DIRECT", "status", "200", "outcome", "SUCCESS")
                .timer().count());
    }

    @Test
    void shutdownOnCloseIsOptInAndOnlyForAutoInitialisedLibraries() throws Exception {
        runner.run(ctx -> {
            assertThat(ctx).doesNotHaveBean("vesperaBridgeShutdown");
            assertFalse(ctx.getBean(VesperaBridgeNativeSettings.class).autoInitialized());
        });
        runner.withPropertyValues("vespera.bridge.shutdown-on-close=true",
                        "vespera.bridge.shutdown-timeout=2s")
                .run(ctx -> {
                    assertEquals(Duration.ofSeconds(2),
                            ctx.getBean(VesperaBridgeProperties.class).getShutdownTimeout());
                    // Not auto-initialised: destroy() must not touch the native side.
                    ((DisposableBean) ctx.getBean("vesperaBridgeShutdown")).destroy();
                });
        VesperaBridgeProperties props = new VesperaBridgeProperties();
        props.setShutdownTimeout(null);
        assertEquals(Duration.ZERO, props.getShutdownTimeout());
        props.setLibraryName("demo");
        props.setShutdownOnClose(true);
        assertEquals("demo", props.getLibraryName());
        assertTrue(props.isShutdownOnClose());
        assertTrue(new VesperaBridgeNativeSettings(true).autoInitialized());
        // An auto-initialised settings bean triggers the (not-loaded → no-op) shutdown.
        new VesperaBridgeCoreAutoConfiguration()
                .vesperaBridgeShutdown(new VesperaBridgeNativeSettings(true), props).destroy();
    }

    @Test
    void autoInitSkipsBlankNamesAndAlreadyInitialisedLibraries() throws Exception {
        assertFalse(VesperaBridgeAutoConfiguration.autoInit(null));
        assertFalse(VesperaBridgeAutoConfiguration.autoInit("  "));
        Field loaded = VesperaBridge.class.getDeclaredField("loaded");
        loaded.setAccessible(true);
        boolean previous = loaded.getBoolean(null);
        try {
            loaded.setBoolean(null, true);
            assertTrue(VesperaBridge.isInitialized());
            assertFalse(VesperaBridgeAutoConfiguration.autoInit("demo"));
            // No cdylib in the unit suite: shutdown0 is unlinked and reported as false.
            assertFalse(VesperaBridge.shutdown(Duration.ofSeconds(-1)));
        } finally {
            loaded.setBoolean(null, previous);
        }
        assertFalse(VesperaBridge.shutdown(Duration.ofSeconds(1)), "not loaded: no-op");
    }
}
