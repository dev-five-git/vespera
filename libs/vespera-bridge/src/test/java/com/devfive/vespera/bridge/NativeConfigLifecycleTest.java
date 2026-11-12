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
