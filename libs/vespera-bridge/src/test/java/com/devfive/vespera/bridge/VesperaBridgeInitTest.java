package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.lang.reflect.Field;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.Test;

/**
 * Q6: {@link VesperaBridge#init(String)} called a second time with a
 * <em>different</em> native library name must fail loudly instead of silently
 * keeping the first library and dispatching to the wrong Rust app; the same
 * name stays a no-op.
 *
 * <p>The mismatch guard runs <em>before</em> any native {@code loadLibrary}, so
 * this test simulates the "already initialised" state via reflection and needs
 * no cdylib. It restores the static state afterwards so it cannot leak into
 * other tests.
 */
class VesperaBridgeInitTest {

    @Test
    void reInitWithDifferentLibraryThrowsAndSameNameIsNoOp() throws Exception {
        Field loadedField = VesperaBridge.class.getDeclaredField("loaded");
        Field nameField = VesperaBridge.class.getDeclaredField("loadedLibraryName");
        loadedField.setAccessible(true);
        nameField.setAccessible(true);
        boolean prevLoaded = loadedField.getBoolean(null);
        Object prevName = nameField.get(null);
        try {
            loadedField.setBoolean(null, true);
            nameField.set(null, "libA");

            assertDoesNotThrow(
                    () -> VesperaBridge.init("libA"),
                    "re-init with the same library name must be a no-op");
            assertThrows(
                    IllegalStateException.class,
                    () -> VesperaBridge.init("libB"),
                    "re-init with a different library name must throw");
        } finally {
            loadedField.setBoolean(null, prevLoaded);
            nameField.set(null, prevName);
        }
    }

    @Test
    void nativeLoadDecisionFallsBackOnlyForAbsentBundledResource() {
        AtomicInteger bundledCalls = new AtomicInteger();
        AtomicInteger systemCalls = new AtomicInteger();
        VesperaBridge.loadNativeLibrary(
                "demo",
                name -> bundledCalls.incrementAndGet(),
                name -> systemCalls.incrementAndGet());
        assertEquals(1, bundledCalls.get());
        assertEquals(0, systemCalls.get());

        VesperaBridge.loadNativeLibrary(
                "fallback",
                name -> { throw new VesperaNativeLoader.BundledNativeAbsent(name); },
                name -> {
                    assertEquals("fallback", name);
                    systemCalls.incrementAndGet();
                });
        assertEquals(1, systemCalls.get());

        UnsatisfiedLinkError invalidBundled = assertThrows(
                UnsatisfiedLinkError.class,
                () -> VesperaBridge.loadNativeLibrary(
                        "invalid",
                        name -> { throw new UnsatisfiedLinkError("invalid bundled"); },
                        name -> systemCalls.incrementAndGet()));
        assertEquals("invalid bundled", invalidBundled.getMessage());
        assertEquals(1, systemCalls.get());
    }

    @Test
    void optionalNativeConfigurationSeamsApplyValuesAndIgnoreMissingSymbols() {
        AtomicInteger configuredChunk = new AtomicInteger();
        AtomicInteger configuredCapacity = new AtomicInteger();
        VesperaBridge.configureStreamingIfSupported(8192, 7, (chunk, capacity) -> {
            configuredChunk.set(chunk);
            configuredCapacity.set(capacity);
        });
        assertEquals(8192, configuredChunk.get());
        assertEquals(7, configuredCapacity.get());
        assertDoesNotThrow(() -> VesperaBridge.configureStreamingIfSupported(
                8192, 7, (chunk, capacity) -> { throw new UnsatisfiedLinkError("old"); }));

        AtomicInteger workers = new AtomicInteger();
        VesperaBridge.configureRuntimeIfSupported(3, workers::set);
        assertEquals(3, workers.get());
        assertDoesNotThrow(() -> VesperaBridge.configureRuntimeIfSupported(
                3, value -> { throw new UnsatisfiedLinkError("old"); }));
    }

    @Test
    void pendingConfigurationOverridesSystemPropertyAndNullUsesProperty() {
        String property = "vespera.test.pending-or-property";
        String previous = System.getProperty(property);
        try {
            System.setProperty(property, "41");
            assertEquals(17, VesperaBridge.pendingOrProperty(17, property));
            assertEquals(41, VesperaBridge.pendingOrProperty(null, property));
            System.setProperty(property, "not-an-integer");
            assertEquals(0, VesperaBridge.pendingOrProperty(null, property));
        } finally {
            if (previous == null) {
                System.clearProperty(property);
            } else {
                System.setProperty(property, previous);
            }
        }
    }
}
