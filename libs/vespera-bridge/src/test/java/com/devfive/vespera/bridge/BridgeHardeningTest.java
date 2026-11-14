package com.devfive.vespera.bridge;

import static org.assertj.core.api.Assertions.assertThat;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.AutoConfigurations;
import org.springframework.boot.test.context.runner.WebApplicationContextRunner;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.web.method.HandlerMethod;
import org.springframework.web.servlet.HandlerExecutionChain;

/**
 * CORS preflight routing, the native buffered-response cap forwarding, the
 * bridge/native version check and the Actuator health indicator — the parts
 * that do not need a loaded cdylib (the E2E suite covers the rest).
 */
class BridgeHardeningTest {

    private final WebApplicationContextRunner runner = new WebApplicationContextRunner()
            .withConfiguration(AutoConfigurations.of(VesperaBridgeCoreAutoConfiguration.class,
                    VesperaBridgeAutoConfiguration.class,
                    VesperaBridgeHealthAutoConfiguration.class));

    @AfterEach
    void clearPendingResponseCap() {
        VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES.clearPendingForTest();
    }

    // ── CORS ────────────────────────────────────────────────────────────

    private static MockHttpServletRequest preflight() {
        MockHttpServletRequest request = new MockHttpServletRequest("OPTIONS", "/users");
        request.addHeader("Origin", "https://app.example");
        request.addHeader("Access-Control-Request-Method", "POST");
        return request;
    }

    @Test
    void preflightMappingHandsOnlyPreflightsToTheProxyMethod() throws Exception {
        VesperaProxyController controller =
                new VesperaProxyController(request -> null, request -> DispatchMode.SYNC);
        VesperaPreflightHandlerMapping mapping = new VesperaPreflightHandlerMapping(controller);

        HandlerExecutionChain chain = mapping.getHandler(preflight());
        HandlerMethod handler = assertInstanceOf(HandlerMethod.class, chain.getHandler());
        assertSame(controller, handler.getBean());
        assertEquals("proxy", handler.getMethod().getName());
        assertEquals(Integer.MIN_VALUE, mapping.getOrder());

        assertNull(mapping.getHandler(new MockHttpServletRequest("OPTIONS", "/users")),
                "a plain OPTIONS is not a preflight");
        MockHttpServletRequest get = new MockHttpServletRequest("GET", "/users");
        get.addHeader("Origin", "https://app.example");
        assertNull(mapping.getHandler(get), "an actual CORS request goes the normal way");
    }

    @Test
    void corsDefaultsToRustAndSpringModeKeepsSpringsPreflightHandling() {
        runner.run(ctx -> {
            assertEquals(CorsHandling.RUST, ctx.getBean(VesperaBridgeProperties.class).getCors());
            assertThat(ctx).hasSingleBean(VesperaPreflightHandlerMapping.class);
        });
        runner.withPropertyValues("vespera.bridge.cors=spring").run(ctx -> {
            assertEquals(CorsHandling.SPRING, ctx.getBean(VesperaBridgeProperties.class).getCors());
            assertThat(ctx).doesNotHaveBean(VesperaPreflightHandlerMapping.class);
        });
        runner.withPropertyValues("vespera.bridge.controller-enabled=false")
                .run(ctx -> assertThat(ctx).doesNotHaveBean(VesperaPreflightHandlerMapping.class));
        VesperaBridgeProperties props = new VesperaBridgeProperties();
        props.setCors(null);
        assertEquals(CorsHandling.RUST, props.getCors());
    }

    // ── buffered-response cap ───────────────────────────────────────────

    private static Long pendingResponseCap() throws Exception {
        Field pending = VesperaNativeConfig.Knob.class.getDeclaredField("pending");
        pending.setAccessible(true);
        return (Long) pending.get(VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES);
    }

    @Test
    void bridgeResponseCapIsForwardedToTheNativeCapUnlessSetExplicitly() throws Exception {
        runner.run(ctx -> assertEquals(
                VesperaProxyController.DEFAULT_MAX_BUFFERED_RESPONSE_BYTES, pendingResponseCap()));
        clearPendingResponseCap();
        runner.withPropertyValues("vespera.bridge.max-buffered-response-bytes=0")
                .run(ctx -> assertEquals(0L, pendingResponseCap(), "0 = unlimited on both sides"));
        clearPendingResponseCap();
        runner.withPropertyValues("vespera.bridge.max-buffered-response-bytes=1000",
                        "vespera.bridge.native.max-buffered-response-bytes=5000")
                .run(ctx -> assertEquals(5000L, pendingResponseCap(), "the native property wins"));
        clearPendingResponseCap();
        System.setProperty("vespera.limits.maxBufferedResponseBytes", "7");
        try {
            runner.run(ctx -> assertNull(pendingResponseCap(), "the system property is left alone"));
        } finally {
            System.clearProperty("vespera.limits.maxBufferedResponseBytes");
        }
    }

    @Test
    void javaPostCheckIsDroppedOnceTheNativeCapIsAtLeastAsStrict() {
        VesperaNativeConfig.Knob knob = VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES;
        assertEquals(64L, VesperaProxyController.javaResponseCap(64), "unknown native cap: keep checking");
        assertTrue(knob.apply(64, value -> true));
        assertEquals(0L, VesperaProxyController.javaResponseCap(64));
        assertEquals(0L, VesperaProxyController.javaResponseCap(100));
        assertEquals(32L, VesperaProxyController.javaResponseCap(32), "native cap looser than Java's");
        assertEquals(0L, VesperaProxyController.javaResponseCap(0));
        assertFalse(knob.apply(128, value -> false), "not applied: the old value stays");
        assertFalse(knob.enforcesAtMost(0));
        assertTrue(knob.enforcesAtMost(64));
    }

    // ── version check ───────────────────────────────────────────────────

    @Test
    void majorMinorParsesReleaseAndPreReleaseVersions() {
        assertEquals("0.6", VesperaVersions.majorMinor("0.6.1"));
        assertEquals("1.2", VesperaVersions.majorMinor("01.2-rc.1"));
        assertEquals("0.6", VesperaVersions.majorMinor(" 0.6 "));
        assertNull(VesperaVersions.majorMinor("dev"));
        assertNull(VesperaVersions.majorMinor("1"));
        assertNull(VesperaVersions.majorMinor("x.1.0"));
        assertNull(VesperaVersions.majorMinor("1.9999999999"));
        assertNull(VesperaVersions.majorMinor(null));
    }

    @Test
    void mismatchOnlyForDifferentReleaseLines() {
        assertNull(VesperaVersions.mismatch("0.6.0", "0.6.3"));
        assertNull(VesperaVersions.mismatch(null, "0.6.3"), "unknown bridge version: nothing to compare");
        assertTrue(VesperaVersions.mismatch("0.6.0", "0.5.9").contains("0.6 vs 0.5"));
        assertTrue(VesperaVersions.mismatch("0.6.0", "garbage").contains("different release lines"));
        assertTrue(VesperaVersions.mismatch("0.6.0", null).contains("does not report its version"));
    }

    @Test
    void verifyFailsStrictlyWarnsOrSkipsByProperty() {
        String bridge = VesperaVersions.bridgeVersion();
        assertTrue(bridge != null && VesperaVersions.majorMinor(bridge) != null,
                "processResources embeds the bridge version: " + bridge);
        byte[] other = "99.0.0".getBytes(StandardCharsets.UTF_8);
        try {
            IllegalStateException failure = assertThrows(IllegalStateException.class,
                    () -> VesperaVersions.verify(() -> other));
            assertTrue(failure.getMessage().contains(VesperaVersions.CHECK_PROPERTY), failure.getMessage());
            assertEquals("99.0.0", VesperaVersions.nativeVersion());

            System.setProperty(VesperaVersions.CHECK_PROPERTY, "warn");
            VesperaVersions.verify(() -> other);
            System.setProperty(VesperaVersions.CHECK_PROPERTY, "off");
            VesperaVersions.verify(() -> other);
            System.clearProperty(VesperaVersions.CHECK_PROPERTY);

            VesperaVersions.verify(() -> bridge.getBytes(StandardCharsets.UTF_8));
            // A native library without nativeVersion0 only warns.
            VesperaVersions.verify(() -> {
                throw new UnsatisfiedLinkError("nativeVersion0");
            });
            assertNull(VesperaVersions.nativeVersion());
            VesperaVersions.verify(() -> new byte[0]);
            VesperaVersions.verify(() -> null);
        } finally {
            System.clearProperty(VesperaVersions.CHECK_PROPERTY);
        }
    }

    // ── health ──────────────────────────────────────────────────────────

    /** Status code of the health indicator bean, via reflection (Boot 3 and Boot 4 types differ). */
    private static String healthStatus(Object indicator, Map<String, Object> detailsOut) throws Exception {
        Method health = indicator.getClass().getMethod("health");
        Object result = health.invoke(indicator);
        Object status = result.getClass().getMethod("getStatus").invoke(result);
        @SuppressWarnings("unchecked")
        Map<String, Object> details = (Map<String, Object>) result.getClass().getMethod("getDetails").invoke(result);
        detailsOut.putAll(details);
        return (String) status.getClass().getMethod("getCode").invoke(status);
    }

    @Test
    void healthIndicatorReportsDownUntilTheLibraryIsLoaded() {
        runner.run(ctx -> {
            assertTrue(ctx.containsBean(VesperaBridgeHealthAutoConfiguration.BEAN_NAME));
            Map<String, Object> details = new java.util.HashMap<>();
            assertEquals("DOWN", healthStatus(
                    ctx.getBean(VesperaBridgeHealthAutoConfiguration.BEAN_NAME), details));
            assertEquals("native library not initialized", details.get("reason"));
            assertEquals(VesperaVersions.bridgeVersion(), details.get("bridgeVersion"));
        });
        runner.withPropertyValues("management.health.vespera.enabled=false")
                .run(ctx -> assertFalse(ctx.containsBean(VesperaBridgeHealthAutoConfiguration.BEAN_NAME)));
    }

    @Test
    void healthIndicatorHonoursTheHealthDefaultsSwitch() {
        // As Boot's @ConditionalOnEnabledHealthIndicator: defaults off disables it ...
        runner.withPropertyValues("management.health.defaults.enabled=false")
                .run(ctx -> assertFalse(ctx.containsBean(VesperaBridgeHealthAutoConfiguration.BEAN_NAME)));
        // ... unless the vespera-specific key is set, which wins either way.
        runner.withPropertyValues("management.health.defaults.enabled=false",
                        "management.health.vespera.enabled=true")
                .run(ctx -> assertTrue(ctx.containsBean(VesperaBridgeHealthAutoConfiguration.BEAN_NAME)));
        runner.withPropertyValues("management.health.defaults.enabled=true",
                        "management.health.vespera.enabled=false")
                .run(ctx -> assertFalse(ctx.containsBean(VesperaBridgeHealthAutoConfiguration.BEAN_NAME)));
    }

    @Test
    void healthStatusFollowsInitAndShutdown() throws Exception {
        Field loaded = VesperaBridge.class.getDeclaredField("loaded");
        Field shutDown = VesperaBridge.class.getDeclaredField("shutDown");
        loaded.setAccessible(true);
        shutDown.setAccessible(true);
        boolean wasLoaded = loaded.getBoolean(null);
        boolean wasShutDown = shutDown.getBoolean(null);
        try {
            assertEquals(VesperaBridgeHealth.Status.DOWN, VesperaBridgeHealth.status());
            loaded.setBoolean(null, true);
            assertEquals(VesperaBridgeHealth.Status.UP, VesperaBridgeHealth.status());
            assertFalse(VesperaBridgeHealth.details().containsKey("reason"));
            shutDown.setBoolean(null, true);
            assertTrue(VesperaBridge.isShutdown());
            assertEquals(VesperaBridgeHealth.Status.OUT_OF_SERVICE, VesperaBridgeHealth.status());
            assertEquals("native runtime shut down", VesperaBridgeHealth.details().get("reason"));
            Map<String, Object> details = new java.util.HashMap<>();
            Object indicator = isBoot4()
                    ? new VesperaBridgeHealthIndicator()
                    : new VesperaBridgeBoot3HealthIndicator();
            assertEquals("OUT_OF_SERVICE", healthStatus(indicator, details));
            shutDown.setBoolean(null, false);
            assertEquals("UP", healthStatus(indicator, details));
        } finally {
            loaded.setBoolean(null, wasLoaded);
            shutDown.setBoolean(null, wasShutDown);
        }
    }

    private static boolean isBoot4() {
        try {
            Class.forName("org.springframework.boot.health.contributor.HealthIndicator");
            return true;
        } catch (ClassNotFoundException boot3) {
            return false;
        }
    }

    @Test
    void asyncEntityStreamsTheWireBodyWithoutCopying() throws Exception {
        byte[] header = "{\"status\":200,\"headers\":{\"content-type\":\"text/plain\"}}"
                .getBytes(StandardCharsets.UTF_8);
        byte[] body = "hello".getBytes(StandardCharsets.UTF_8);
        byte[] wire = java.nio.ByteBuffer.allocate(4 + header.length + body.length)
                .putInt(header.length).put(header).put(body).array();
        org.springframework.http.ResponseEntity<?> entity =
                WireResponseWriter.buildAsyncResponseEntityFromWire(wire, "GET", null);
        org.springframework.core.io.InputStreamResource resource = assertInstanceOf(
                org.springframework.core.io.InputStreamResource.class, entity.getBody());
        assertSame(org.springframework.core.io.InputStreamResource.class, resource.getClass(),
                "exactly InputStreamResource: Spring skips Range handling for it");
        assertEquals("hello", new String(resource.getInputStream().readAllBytes(), StandardCharsets.UTF_8));
        assertEquals(5, entity.getHeaders().getContentLength());

        org.springframework.http.ResponseEntity<?> head =
                WireResponseWriter.buildAsyncResponseEntityFromWire(wire, "HEAD", null);
        assertEquals(0, ((byte[]) head.getBody()).length);
        assertEquals(List.of("5"), List.of(head.getHeaders().getFirst("Content-Length")));
    }
}
