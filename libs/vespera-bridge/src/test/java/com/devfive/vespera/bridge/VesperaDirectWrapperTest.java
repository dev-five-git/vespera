package com.devfive.vespera.bridge;

import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotSame;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Pure-Java tests for the {@code dispatchDirect} wrapper's pre-JNI
 * validation — no native library is loaded.  Every rejection asserted
 * here MUST happen before the native method is invoked; if validation
 * regressed and the call crossed JNI, these tests would fail with
 * {@link UnsatisfiedLinkError} instead of the expected exception.
 */
class VesperaDirectWrapperTest {

    private static final ByteBuffer DIRECT = ByteBuffer.allocateDirect(64);
    private static final ByteBuffer HEAP = ByteBuffer.allocate(64);

    @Test
    void heapInBufferRejectedBeforeJni() {
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.dispatchDirect(HEAP, 4, DIRECT));
        assertTrue(e.getMessage().contains("direct"), e.getMessage());
    }

    @Test
    void heapOutBufferRejectedBeforeJni() {
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.dispatchDirect(DIRECT, 4, HEAP));
        assertTrue(e.getMessage().contains("direct"), e.getMessage());
    }

    @Test
    void nullBuffersRejected() {
        assertThrows(NullPointerException.class,
                () -> VesperaBridge.dispatchDirect(null, 0, DIRECT));
        assertThrows(NullPointerException.class,
                () -> VesperaBridge.dispatchDirect(DIRECT, 0, null));
    }

    @Test
    void negativeInLenRejected() {
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.dispatchDirect(DIRECT, -1, DIRECT));
        assertTrue(e.getMessage().contains("inLen"), e.getMessage());
    }

    @Test
    void inLenBeyondCapacityRejected() {
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.dispatchDirect(DIRECT, DIRECT.capacity() + 1, DIRECT));
        assertTrue(e.getMessage().contains("inLen"), e.getMessage());
    }

    @Test
    void readOnlyOutBufferRejectedBeforeJni() {
        // SEC-2: a read-only direct out buffer would crash the native
        // write; the wrapper must reject it before crossing JNI.
        ByteBuffer readOnlyOut = ByteBuffer.allocateDirect(64).asReadOnlyBuffer();
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.dispatchDirect(DIRECT, 4, readOnlyOut));
        assertTrue(e.getMessage().contains("writable"), e.getMessage());
    }

    @Test
    void readOnlyInBufferRejectedBeforeJni() {
        ByteBuffer readOnlyIn = ByteBuffer.allocateDirect(64).asReadOnlyBuffer();
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.dispatchDirect(readOnlyIn, 4, DIRECT));
        assertTrue(e.getMessage().contains("writable"), e.getMessage());
    }

    @Test
    void bufferTooSmallExceptionCarriesRequiredSize() {
        VesperaBridge.BufferTooSmallException e =
                new VesperaBridge.BufferTooSmallException(123_456);
        assertEquals(123_456, e.requiredSize());
        assertTrue(e.getMessage().contains("123456"), e.getMessage());
        assertTrue(e.getMessage().contains("re-run"), e.getMessage());
    }

    @Test
    void integerMinValueDirectOverflowHasActionableMessage() {
        IllegalStateException e = VesperaDirectBufferPool.responseExceedsTwoGiBException();

        assertTrue(e.getMessage().contains("exceeds 2 GiB"), e.getMessage());
        assertTrue(e.getMessage().contains("streaming dispatch"), e.getMessage());
    }

    @Test
    void directPoolKeepsBaselineBuffersAfterIdleStreak() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        ByteBuffer[] pool = VesperaDirectBufferPool.directPoolForTest();

        for (int i = 0; i < 8; i++) {
            VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1);
        }

        assertTrue(VesperaDirectBufferPool.directPoolPresentForTest());
        assertEquals(64 * 1024, pool[0].capacity());
        assertEquals(64 * 1024, pool[1].capacity());
    }

    @Test
    void directPoolShrinksGrownBuffersAfterIdleStreak() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        ByteBuffer[] pool = VesperaDirectBufferPool.directPoolForTest();
        pool[0] = ByteBuffer.allocateDirect(3 * 1024 * 1024);
        pool[1] = ByteBuffer.allocateDirect(3 * 1024 * 1024);

        for (int i = 0; i < 8; i++) {
            VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1);
        }

        assertTrue(VesperaDirectBufferPool.directPoolPresentForTest());
        assertEquals(64 * 1024, pool[0].capacity());
        assertEquals(64 * 1024, pool[1].capacity());
    }

    @Test
    void directPoolRetainsMediumResponseUnderRetainCapAfterIdleStreak() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        ByteBuffer[] pool = VesperaDirectBufferPool.directPoolForTest();
        pool[1] = ByteBuffer.allocateDirect(1024 * 1024);

        for (int i = 0; i < 9; i++) {
            VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1024 * 1024);
        }

        assertTrue(VesperaDirectBufferPool.directPoolPresentForTest());
        assertEquals(64 * 1024, pool[0].capacity());
        assertEquals(1024 * 1024, pool[1].capacity());
    }

    @Test
    void directPoolAllocatesOnceReusesAndCanBeCleared() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        assertFalse(VesperaDirectBufferPool.directPoolPresentForTest());

        ByteBuffer[] first = VesperaDirectBufferPool.directPoolForTest();
        ByteBuffer[] reused = VesperaDirectBufferPool.directPoolForTest();

        assertSame(first, reused);
        assertTrue(first[0].isDirect());
        assertTrue(first[1].isDirect());
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        assertFalse(VesperaDirectBufferPool.directPoolPresentForTest());
        assertNotSame(first, VesperaDirectBufferPool.directPoolForTest());
    }

    @Test
    void largeUseResetsShrinkStreakBeforeAdaptiveShrink() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        ByteBuffer[] pool = VesperaDirectBufferPool.directPoolForTest();
        pool[0] = ByteBuffer.allocateDirect(3 * 1024 * 1024);

        for (int i = 0; i < 7; i++) {
            VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1);
        }
        VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 3 * 1024 * 1024, 1);
        VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1);

        assertEquals(3 * 1024 * 1024, pool[0].capacity());
    }

    @Test
    void requestBufferGrowsBeforeMissingNativeDispatchIsObserved() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        byte[] request = new byte[64 * 1024 + 1];

        assertThrows(UnsatisfiedLinkError.class,
                () -> VesperaDirectBufferPool.dispatchDirectPooled(request, false, false));

        ByteBuffer[] pool = VesperaDirectBufferPool.directPoolForTest();
        assertEquals(128 * 1024, pool[0].capacity());
        assertEquals(request.length, pool[0].position());
    }

    @Test
    void convenienceOverloadsDetectPlatformThreadBeforeReachingNativeBoundary() {
        assertThrows(NullPointerException.class,
                () -> VesperaDirectBufferPool.dispatchDirectPooled((byte[]) null, false));

        VesperaBridge.HeaderSource headers = sink -> sink.put("x-test", "yes");
        assertThrows(UnsatisfiedLinkError.class, () -> VesperaDirectBufferPool.dispatchDirectPooled(
                null, "GET", "/items", null, headers, null, false));
    }

    @Test
    void encodedMapAndHeaderSourceRequestsGrowAndReachNativeBoundary() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        byte[] body = new byte[70 * 1024];
        assertThrows(UnsatisfiedLinkError.class, () -> VesperaDirectBufferPool.dispatchDirectPooled(
                "admin", "POST", "/upload", "a=1", Map.of("x-test", "yes"), body, false));
        assertTrue(VesperaDirectBufferPool.directPoolForTest()[0].capacity() >= body.length);

        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        VesperaBridge.HeaderSource headers = sink -> sink.put("x-test", "yes");
        assertThrows(UnsatisfiedLinkError.class, () -> VesperaDirectBufferPool.dispatchDirectPooled(
                null, "GET", "/items", null, headers, null, false, false));
        assertTrue(VesperaDirectBufferPool.directPoolPresentForTest());
    }

    @Test
    void nullRequestAndThrowOversizePolicyRejectBeforeNativeDispatch() {
        assertThrows(NullPointerException.class,
                () -> VesperaDirectBufferPool.dispatchDirectPooled(null, false, false));
        String previous = System.getProperty("vespera.direct.oversize-policy");
        try {
            System.setProperty("vespera.direct.oversize-policy", "throw");
            VesperaBridge.BufferTooSmallException virtual = assertThrows(
                    VesperaBridge.BufferTooSmallException.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(new byte[17], false, true));
            assertEquals(17, virtual.requiredSize());
            assertTrue(virtual.getMessage().contains("virtual thread"), virtual.getMessage());

            VesperaBridge.BufferTooSmallException encoded = assertThrows(
                    VesperaBridge.BufferTooSmallException.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(
                            null, "GET", "/", null,
                            (VesperaBridge.HeaderSource) sink -> {}, null, false, true));
            assertTrue(encoded.requiredSize() > 0);

            VesperaBridge.HeaderSource headers = sink -> sink.put("x-test", "yes");
            assertThrows(VesperaBridge.BufferTooSmallException.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(
                            null, "GET", "/", null, headers, null, false, true));
        } finally {
            restoreProperty("vespera.direct.oversize-policy", previous);
        }
    }

    @Test
    void invalidOversizePolicyIsRejectedAndHeapFallbackReachesHeapNativeBoundary() {
        String previous = System.getProperty("vespera.direct.oversize-policy");
        try {
            System.setProperty("vespera.direct.oversize-policy", "invalid");
            IllegalArgumentException invalid = assertThrows(
                    IllegalArgumentException.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(new byte[1], false, true));
            assertTrue(invalid.getMessage().contains("heap-fallback"), invalid.getMessage());

            System.setProperty("vespera.direct.oversize-policy", "HeAp-FaLlBaCk");
            assertThrows(UnsatisfiedLinkError.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(new byte[1], false, true));

            VesperaBridge.HeaderSource headers = sink -> sink.put("x-test", "yes");
            assertThrows(UnsatisfiedLinkError.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(
                            null, "GET", "/", null, headers, null, false, true));
        } finally {
            restoreProperty("vespera.direct.oversize-policy", previous);
        }
    }

    @Test
    void oversizedEncodedMapRequestHonorsThrowAndHeapFallbackPolicies() {
        String previous = System.getProperty("vespera.direct.oversize-policy");
        byte[] oversized = new byte[4 * 1024 * 1024 + 1];
        try {
            System.setProperty("vespera.direct.oversize-policy", "throw");
            VesperaBridge.BufferTooSmallException rejected = assertThrows(
                    VesperaBridge.BufferTooSmallException.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(
                            null, "POST", "/upload", null, Map.of(), oversized, false));
            assertTrue(rejected.requiredSize() > oversized.length);

            System.setProperty("vespera.direct.oversize-policy", "heap-fallback");
            assertThrows(UnsatisfiedLinkError.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(
                            null, "POST", "/upload", null, Map.of(), oversized, false));
        } finally {
            restoreProperty("vespera.direct.oversize-policy", previous);
        }
    }

    @Test
    void growthRoundsUpAndStopsAtConfiguredMaximum() throws Exception {
        Method grownCapacity = VesperaDirectBufferPool.class.getDeclaredMethod("grownCapacity", int.class);
        grownCapacity.setAccessible(true);

        assertEquals(64 * 1024, grownCapacity.invoke(null, 1));
        assertEquals(4 * 1024 * 1024, grownCapacity.invoke(null, 3 * 1024 * 1024));
        assertEquals(5 * 1024 * 1024, grownCapacity.invoke(null, 5 * 1024 * 1024));
    }

    @Test
    void configuredMaximumIsClampedAtBothBoundsAndPropertyIsRestored() throws Exception {
        String previous = System.getProperty("vespera.direct.maxBufferBytes");
        try {
            System.setProperty("vespera.direct.maxBufferBytes", "1");
            assertEquals(64 * 1024, isolatedDirectMaximum());

            System.setProperty("vespera.direct.maxBufferBytes", Integer.toString(300 * 1024 * 1024));
            assertEquals(256 * 1024 * 1024, isolatedDirectMaximum());
        } finally {
            restoreProperty("vespera.direct.maxBufferBytes", previous);
        }
    }

    private static int isolatedDirectMaximum() throws Exception {
        URL classes = VesperaBridge.class.getProtectionDomain().getCodeSource().getLocation();
        try (URLClassLoader loader = new URLClassLoader(
                new URL[] {classes}, VesperaDirectWrapperTest.class.getClassLoader()) {
            @Override
            protected Class<?> loadClass(String name, boolean resolve) throws ClassNotFoundException {
                if (name.startsWith("com.devfive.vespera.bridge.VesperaDirectBufferPool")) {
                    synchronized (getClassLoadingLock(name)) {
                        Class<?> loaded = findLoadedClass(name);
                        if (loaded == null) {
                            loaded = findClass(name);
                        }
                        if (resolve) {
                            resolveClass(loaded);
                        }
                        return loaded;
                    }
                }
                return super.loadClass(name, resolve);
            }
        }) {
            Class<?> isolated = Class.forName(
                    "com.devfive.vespera.bridge.VesperaDirectBufferPool", true, loader);
            Field maximum = isolated.getDeclaredField("DIRECT_MAX_CAPACITY");
            maximum.setAccessible(true);
            return maximum.getInt(null);
        }
    }

    @Test
    void platformThreadDetectionMatchesJavaSeventeenRuntime() {
        assertFalse(VesperaDirectBufferPool.currentThreadIsVirtual());
    }

    @Test
    void headerSourcePooledWrappersValidateInputsBeforeNativeDispatch() {
        VesperaBridge.HeaderSource headers = sink -> sink.put("x-test", "yes");

        NullPointerException missingMethod = assertThrows(
                NullPointerException.class,
                () -> VesperaBridge.dispatchDirectPooled(
                        null, null, "/items", null, headers, null, false));
        assertEquals("method", missingMethod.getMessage());

        IllegalArgumentException queryInPath = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.dispatchDirectPooled(
                        null, "GET", "/items?a=1", null, headers, null, false, false));
        assertEquals(
                "path must not contain '?' — pass the raw query string via the query parameter",
                queryInPath.getMessage());
    }

    private static void restoreProperty(String name, String value) {
        if (value == null) {
            System.clearProperty(name);
        } else {
            System.setProperty(name, value);
        }
    }
}
