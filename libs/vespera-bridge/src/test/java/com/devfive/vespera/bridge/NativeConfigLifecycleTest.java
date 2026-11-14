package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

/** Runtime start hook + upload-producer knob of {@link VesperaNativeConfig}. */
class NativeConfigLifecycleTest {

    @Test
    void startRuntimeToleratesAnOlderNativeLibrary() {
        assertTrue(VesperaNativeConfig.startRuntimeIfNeeded(() -> true));
        assertFalse(VesperaNativeConfig.startRuntimeIfNeeded(() -> false));
        assertFalse(VesperaNativeConfig.startRuntimeIfNeeded(() -> {
            throw new UnsatisfiedLinkError("startRuntime0");
        }));
        // No native library in unit tests: the real symbol is missing too.
        assertFalse(VesperaNativeConfig.startRuntimeIfNeeded(VesperaNativeConfig::startRuntime0));
    }

    @Test
    void aValueAppliedAfterInitCountsAsExplicitlyConfigured() {
        VesperaNativeConfig.Knob knob = new VesperaNativeConfig.Knob(
                "test.cap", "vespera.test.round6.cap", -1, value -> true);
        assertFalse(knob.explicitlyConfigured());
        // init(...) already loaded the library: applied directly, never pending.
        assertTrue(knob.set(512L * 1024 * 1024, true));
        assertTrue(knob.explicitlyConfigured(),
                "a programmatic configureLimits after init must not be overridden by Spring's default");
        assertTrue(knob.enforcesAtMost(1024L * 1024 * 1024));

        VesperaNativeConfig.Knob rejected = new VesperaNativeConfig.Knob(
                "test.cap", "vespera.test.round6.cap", -1, value -> false);
        assertFalse(rejected.set(5, true));
        assertFalse(rejected.explicitlyConfigured(), "a value the native side refused is not configured");
        assertTrue(rejected.set(-1, true), "unset sentinel: no-op");
        assertFalse(rejected.explicitlyConfigured());
    }

    @Test
    void shutdownTimeoutIsClampedInsteadOfOverflowing() {
        assertEquals(0, VesperaBridge.shutdownTimeoutMillis(java.time.Duration.ofSeconds(-1)));
        assertEquals(1500, VesperaBridge.shutdownTimeoutMillis(java.time.Duration.ofMillis(1500)));
        assertEquals(Long.MAX_VALUE, VesperaBridge.shutdownTimeoutMillis(
                java.time.temporal.ChronoUnit.FOREVER.getDuration()));
        assertEquals(Long.MAX_VALUE, VesperaBridge.shutdownTimeoutMillis(
                java.time.Duration.ofSeconds(Long.MAX_VALUE / 1000 + 1)));
    }

    @Test
    void requestProducerKnobUsesTheStreamingProperty() {
        VesperaNativeConfig.Knob knob = VesperaNativeConfig.MAX_REQUEST_PRODUCERS;
        assertEquals("vespera.streaming.maxRequestProducers", knob.property);
        assertTrue(knob.isUnset(-1));
        assertFalse(knob.isUnset(0), "0 = unlimited is a real value");
        String previous = System.getProperty(knob.property);
        try {
            System.setProperty(knob.property, "8");
            assertEquals(8, knob.resolve());
            // Missing native hook: reported as not applied, never thrown.
            assertFalse(knob.apply(8, VesperaNativeConfig::configureMaxRequestProducers0));
        } finally {
            if (previous == null) {
                System.clearProperty(knob.property);
            } else {
                System.setProperty(knob.property, previous);
            }
        }
    }
}
