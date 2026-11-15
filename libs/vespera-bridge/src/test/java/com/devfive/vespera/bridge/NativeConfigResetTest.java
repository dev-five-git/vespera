package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;

import org.junit.jupiter.api.Test;

/**
 * {@link NativeConfigReset} clears what a context-booting test leaves in the
 * static native-config knobs, so the next class starts unconfigured.
 */
class NativeConfigResetTest {

    @Test
    void clearsEveryLeakedKnobAndTheStreamingPair() {
        List<VesperaNativeConfig.Knob> knobs = NativeConfigReset.knobs();
        assertTrue(knobs.contains(VesperaNativeConfig.MAX_BUFFERED_RESPONSE_BYTES), knobs.toString());
        assertTrue(knobs.contains(VesperaNativeConfig.RUNTIME_WORKER_THREADS), knobs.toString());
        assertTrue(knobs.contains(VesperaNativeConfig.DEFAULT_JSON_CONTENT_TYPE), knobs.toString());
        try {
            // What booting VesperaBridgeCoreAutoConfiguration leaves behind.
            for (VesperaNativeConfig.Knob knob : knobs) {
                assertTrue(knob.set(64L * 1024 * 1024, false), knob.label);
                assertTrue(knob.explicitlyConfigured(), knob.label);
            }
            assertTrue(VesperaNativeConfig.setStreaming(64 * 1024, 4, false));

            new NativeConfigReset().beforeEach(null);

            for (VesperaNativeConfig.Knob knob : knobs) {
                if (System.getProperty(knob.property) == null) {
                    assertFalse(knob.explicitlyConfigured(), knob.label);
                }
            }
            assertNull(VesperaNativeConfig.pendingStreamingForTest());
        } finally {
            NativeConfigReset.clearAll();
        }
    }
}
