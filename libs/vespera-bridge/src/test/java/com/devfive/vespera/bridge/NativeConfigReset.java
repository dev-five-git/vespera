package com.devfive.vespera.bridge;

import java.lang.reflect.Field;
import java.lang.reflect.Modifier;
import java.util.ArrayList;
import java.util.List;

import org.junit.jupiter.api.extension.AfterEachCallback;
import org.junit.jupiter.api.extension.BeforeEachCallback;
import org.junit.jupiter.api.extension.ExtensionContext;

/**
 * Clears every static {@link VesperaNativeConfig} knob (pending and confirmed
 * values, plus the pending streaming pair) before and after each test.
 *
 * <p>The bridge tests share one JVM, and booting
 * {@link VesperaBridgeCoreAutoConfiguration} forwards values into these
 * static knobs (the buffered-response cap among them). A value one class
 * leaves behind makes {@code explicitlyConfigured()} true for the next, so a
 * forwarding assertion there could pass without forwarding anything. Every
 * context-booting test class registers this extension; the knobs are found
 * reflectively, so a knob added later is cleared too.
 */
final class NativeConfigReset implements BeforeEachCallback, AfterEachCallback {

    @Override
    public void beforeEach(ExtensionContext context) {
        clearAll();
    }

    @Override
    public void afterEach(ExtensionContext context) {
        clearAll();
    }

    /** Every {@code static final Knob} field of {@link VesperaNativeConfig}. */
    static List<VesperaNativeConfig.Knob> knobs() {
        List<VesperaNativeConfig.Knob> knobs = new ArrayList<>();
        for (Field field : VesperaNativeConfig.class.getDeclaredFields()) {
            if (Modifier.isStatic(field.getModifiers())
                    && field.getType() == VesperaNativeConfig.Knob.class) {
                try {
                    field.setAccessible(true);
                    knobs.add((VesperaNativeConfig.Knob) field.get(null));
                } catch (IllegalAccessException e) {
                    throw new IllegalStateException("cannot read " + field, e);
                }
            }
        }
        return knobs;
    }

    static void clearAll() {
        for (VesperaNativeConfig.Knob knob : knobs()) {
            knob.clearPendingForTest();
        }
        VesperaNativeConfig.clearPendingStreamingForTest();
    }
}
