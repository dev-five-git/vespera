package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.lang.reflect.Field;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;
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
        Field nativeField = VesperaBridge.class.getDeclaredField("nativeLoaded");
        Field loadedField = VesperaBridge.class.getDeclaredField("loaded");
        Field nameField = VesperaBridge.class.getDeclaredField("loadedLibraryName");
        nativeField.setAccessible(true);
        loadedField.setAccessible(true);
        nameField.setAccessible(true);
        boolean prevNative = nativeField.getBoolean(null);
        boolean prevLoaded = loadedField.getBoolean(null);
        Object prevName = nameField.get(null);
        try {
            // A fully configured library: loaded and configured.
            nativeField.setBoolean(null, true);
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
            nativeField.setBoolean(null, prevNative);
            loadedField.setBoolean(null, prevLoaded);
            nameField.set(null, prevName);
        }
    }

    @Test
    void isInitializedIsPublishedOnlyAfterThePendingConfigurationWasApplied() throws Exception {
        Field loadedField = VesperaBridge.class.getDeclaredField("loaded");
        Field nativeField = VesperaBridge.class.getDeclaredField("nativeLoaded");
        Field nameField = VesperaBridge.class.getDeclaredField("loadedLibraryName");
        loadedField.setAccessible(true);
        nativeField.setAccessible(true);
        nameField.setAccessible(true);
        boolean prevLoaded = loadedField.getBoolean(null);
        boolean prevNative = nativeField.getBoolean(null);
        Object prevName = nameField.get(null);
        try {
            AtomicInteger applied = new AtomicInteger();
            VesperaBridge.completeLoad("libA", () -> {
                // A concurrent dispatcher gating on isInitialized() must not
                // run before the pending configure* values reach Rust ...
                assertFalse(VesperaBridge.isInitialized());
                // ... while configure* already treats the library as loaded.
                // A re-init in that window is refused: the configuration has
                // not completed yet.
                assertThrows(IllegalStateException.class, () -> VesperaBridge.init("libA"));
                applied.incrementAndGet();
            });
            assertEquals(1, applied.get());
            assertTrue(VesperaBridge.isInitialized());

            // A failing configuration hook propagates and does NOT publish
            // the library as initialised: dispatchers stay closed.
            loadedField.setBoolean(null, false);
            assertThrows(IllegalStateException.class, () -> VesperaBridge.completeLoad("libA", () -> {
                throw new IllegalStateException("hook failed");
            }));
            assertFalse(VesperaBridge.isInitialized());
            // The cdylib stays marked loaded (never loaded twice), so a later
            // init() reports the incomplete configuration instead of no-op.
            IllegalStateException again = assertThrows(
                    IllegalStateException.class, () -> VesperaBridge.init("libA"));
            assertTrue(again.getMessage().contains("restart the JVM"), again.getMessage());
            assertFalse(VesperaBridge.isInitialized());
        } finally {
            loadedField.setBoolean(null, prevLoaded);
            nativeField.setBoolean(null, prevNative);
            nameField.set(null, prevName);
        }
    }

    @Test
    void nativeLoadDecisionFallsBackOnlyForAbsentBundledResource() {
        AtomicInteger bundledCalls = new AtomicInteger();
        AtomicInteger systemCalls = new AtomicInteger();
        VesperaNativeLoader.loadNativeLibrary(
                "demo",
                name -> bundledCalls.incrementAndGet(),
                name -> systemCalls.incrementAndGet());
        assertEquals(1, bundledCalls.get());
        assertEquals(0, systemCalls.get());

        VesperaNativeLoader.loadNativeLibrary(
                "fallback",
                name -> { throw new VesperaNativeLoader.BundledNativeAbsent(name); },
                name -> {
                    assertEquals("fallback", name);
                    systemCalls.incrementAndGet();
                });
        assertEquals(1, systemCalls.get());

        UnsatisfiedLinkError invalidBundled = assertThrows(
                UnsatisfiedLinkError.class,
                () -> VesperaNativeLoader.loadNativeLibrary(
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
        assertTrue(VesperaNativeConfig.configureStreamingIfSupported(8192, 7, (chunk, capacity) -> {
            configuredChunk.set(chunk);
            configuredCapacity.set(capacity);
            return true;
        }));
        assertEquals(8192, configuredChunk.get());
        assertEquals(7, configuredCapacity.get());
        assertFalse(VesperaNativeConfig.configureStreamingIfSupported(
                8192, 7, (chunk, capacity) -> false),
                "a value the Rust side already fixed is surfaced as false");
        assertFalse(assertDoesNotThrow(() -> VesperaNativeConfig.configureStreamingIfSupported(
                8192, 7, (chunk, capacity) -> { throw new UnsatisfiedLinkError("old"); })));

        VesperaNativeConfig.Knob knob = new VesperaNativeConfig.Knob(
                "test.knob", "vespera.test.knob", -1, value -> true);
        AtomicLong applied = new AtomicLong(Long.MIN_VALUE);
        assertTrue(knob.apply(3, value -> { applied.set(value); return true; }));
        assertEquals(3, applied.get());
        assertFalse(knob.apply(3, value -> false));
        assertFalse(assertDoesNotThrow(
                () -> knob.apply(3, value -> { throw new UnsatisfiedLinkError("old"); })));
        assertFalse(assertDoesNotThrow(
                () -> knob.apply(-1, value -> { throw new UnsatisfiedLinkError("old"); })),
                "an unset value on an old native library is a quiet no-op");
    }

    @Test
    void unsetSentinelInConfigureRuntimeDoesNotShadowTheSystemProperty() {
        String property = "vespera.runtime.maxBlockingThreads";
        String previous = System.getProperty(property);
        VesperaNativeConfig.Knob workers = VesperaNativeConfig.RUNTIME_WORKER_THREADS;
        VesperaNativeConfig.Knob blocking = VesperaNativeConfig.RUNTIME_MAX_BLOCKING_THREADS;
        try {
            System.setProperty(property, "77");
            assertTrue(VesperaBridge.configureRuntime(8, 0));
            assertEquals(8, workers.resolve(), "the programmatic worker count is pending");
            assertEquals(77, blocking.resolve(),
                    "maxBlockingThreads=0 (unset) must fall through to the system property");
            assertTrue(blocking.set(-5, false), "negative is unset too");
            assertEquals(77, blocking.resolve());
            assertTrue(VesperaNativeConfig.MAX_REQUEST_BYTES.isUnset(-1));
            assertFalse(VesperaNativeConfig.MAX_REQUEST_BYTES.isUnset(0),
                    "0 means unlimited for the limit knobs, not unset");
        } finally {
            workers.clearPendingForTest();
            blocking.clearPendingForTest();
            if (previous == null) {
                System.clearProperty(property);
            } else {
                System.setProperty(property, previous);
            }
        }
    }

    @Test
    void knobsStorePendingValuesBeforeInitAndPreferThemOverProperties() {
        String property = "vespera.test.knob.pending";
        String previous = System.getProperty(property);
        try {
            VesperaNativeConfig.Knob knob = new VesperaNativeConfig.Knob(
                    "test.knob", property, -1, value -> {
                        throw new AssertionError("must not reach native before init");
                    });
            assertEquals(-1, knob.resolve(), "unset without property or pending value");
            System.setProperty(property, "42");
            assertEquals(42, knob.resolve(), "system property applies");
            assertTrue(knob.set(0, false), "pre-init values are stored");
            assertEquals(0, knob.resolve(), "pending 0 (unlimited) beats the property");
            knob.clearPendingForTest();
            assertEquals(42, knob.resolve());
        } finally {
            if (previous == null) {
                System.clearProperty(property);
            } else {
                System.setProperty(property, previous);
            }
        }
        assertTrue(new VesperaNativeConfig.Knob("t", "t", -1, v -> true).isUnset(-1));
        assertEquals(Integer.MAX_VALUE, VesperaNativeConfig.toInt(Long.MAX_VALUE));
        assertEquals(-1, VesperaNativeConfig.toInt(-1));
    }

    /** A malformed knob value is not silently dropped: a WARNING names the property. */
    @Test
    void malformedKnobPropertyWarnsAndFallsBack() {
        String property = "vespera.test.malformed-knob";
        java.util.logging.Logger jul = java.util.logging.Logger.getLogger(VesperaBridge.class.getName());
        java.util.List<String> warnings = new java.util.concurrent.CopyOnWriteArrayList<>();
        java.util.logging.Handler capture = new java.util.logging.Handler() {
            @Override
            public void publish(java.util.logging.LogRecord record) {
                if (record.getLevel() == java.util.logging.Level.WARNING) {
                    warnings.add(record.getMessage());
                }
            }

            @Override
            public void flush() {}

            @Override
            public void close() {}
        };
        jul.addHandler(capture);
        try {
            System.setProperty(property, "64MiB");
            assertEquals(-1, new VesperaNativeConfig.Knob("t", property, -1, v -> true).resolve());
            assertEquals(0, VesperaNativeConfig.pendingOrProperty(null, property));
            assertEquals(2, warnings.size(), warnings.toString());
            assertTrue(warnings.get(0).contains(property) && warnings.get(0).contains("64MiB"), warnings.get(0));

            warnings.clear();
            System.setProperty(property, " 0x10 ");
            assertEquals(16, VesperaNativeConfig.longProperty(property, 7));
            System.clearProperty(property);
            assertEquals(7, VesperaNativeConfig.longProperty(property, 7));
            assertTrue(warnings.isEmpty(), warnings.toString());

            // A zero-padded decimal is decimal (Long.decode would read 0100 as octal 64).
            System.setProperty(property, "0100");
            assertEquals(100, VesperaNativeConfig.longProperty(property, 7));
            System.setProperty(property, "-010");
            assertEquals(-10, VesperaNativeConfig.longProperty(property, 7));
            System.setProperty(property, "0x0100");
            assertEquals(256, VesperaNativeConfig.longProperty(property, 7));
            System.setProperty(property, "#ff");
            assertEquals(255, VesperaNativeConfig.longProperty(property, 7));
            System.setProperty(property, "0x-5");
            assertEquals(7, VesperaNativeConfig.longProperty(property, 7));
            assertEquals(1, warnings.size(), warnings.toString());
            assertTrue(warnings.get(0).contains(property), warnings.get(0));
        } finally {
            jul.removeHandler(capture);
            System.clearProperty(property);
        }
    }

    @Test
    void pendingConfigurationOverridesSystemPropertyAndNullUsesProperty() {
        String property = "vespera.test.pending-or-property";
        String previous = System.getProperty(property);
        try {
            System.setProperty(property, "41");
            assertEquals(17, VesperaNativeConfig.pendingOrProperty(17, property));
            assertEquals(41, VesperaNativeConfig.pendingOrProperty(null, property));
            System.setProperty(property, "not-an-integer");
            assertEquals(0, VesperaNativeConfig.pendingOrProperty(null, property));
        } finally {
            if (previous == null) {
                System.clearProperty(property);
            } else {
                System.setProperty(property, previous);
            }
        }
    }
}
