package com.devfive.vespera.bridge;

import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.Map;
import java.util.concurrent.atomic.AtomicInteger;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
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
    void largeResponseAloneResetsShrinkStreakBeforeAdaptiveShrink() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        ByteBuffer[] pool = VesperaDirectBufferPool.directPoolForTest();
        pool[1] = ByteBuffer.allocateDirect(3 * 1024 * 1024);

        for (int i = 0; i < 7; i++) {
            VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1);
        }
        VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 3 * 1024 * 1024);
        VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1);

        assertEquals(3 * 1024 * 1024, pool[1].capacity());
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

        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        byte[] sourceBody = new byte[70 * 1024];
        assertThrows(UnsatisfiedLinkError.class, () -> VesperaDirectBufferPool.dispatchDirectPooled(
                null, "POST", "/source-upload", null,
                headers, sourceBody, false, false));
        assertTrue(VesperaDirectBufferPool.directPoolForTest()[0].capacity() >= sourceBody.length);
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

        assertThrows(UnsatisfiedLinkError.class,
                () -> VesperaBridge.dispatchDirectPooled(
                        null, "GET", "/items", null, headers, null, false));

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

    @Test
    void virtualThreadReflectionSeamCoversResolutionAndFailurePolicies() throws Exception {
        assertFalse(VesperaDirectBufferPool.invokeThreadBooleanMethod(null, Thread.currentThread()));
        assertTrue(VesperaDirectBufferPool.invokeThreadBooleanMethod(
                VesperaDirectBufferPool.resolveThreadBooleanMethod("isAlive"),
                Thread.currentThread()));
        assertEquals(null, VesperaDirectBufferPool.resolveThreadBooleanMethod("missingMethod"));

        var lookup = java.lang.invoke.MethodHandles.lookup();
        var signature = java.lang.invoke.MethodType.methodType(boolean.class, Thread.class);
        var runtimeFailure = lookup.findStatic(
                VesperaDirectWrapperTest.class, "throwRuntime", signature);
        IllegalStateException runtime = assertThrows(
                IllegalStateException.class,
                () -> VesperaDirectBufferPool.invokeThreadBooleanMethod(
                        runtimeFailure, Thread.currentThread()));
        assertEquals("runtime", runtime.getMessage());

        var checkedFailure = lookup.findStatic(
                VesperaDirectWrapperTest.class, "throwChecked", signature);
        assertFalse(VesperaDirectBufferPool.invokeThreadBooleanMethod(
                checkedFailure, Thread.currentThread()));
    }

    @Test
    void pooledFallbackAndAssemblyDecisionSeamsPreserveExactResults() {
        assertEquals("virtual thread", VesperaDirectBufferPool.pooledFallbackReason(true));
        assertEquals("oversized request", VesperaDirectBufferPool.pooledFallbackReason(false));
        VesperaDirectBufferPool.requireExpectedWrite(7, 7);
        IllegalStateException mismatch = assertThrows(
                IllegalStateException.class,
                () -> VesperaDirectBufferPool.requireExpectedWrite(6, 7));
        assertEquals("assembleInto wrote 6, expected 7", mismatch.getMessage());

        ByteBuffer heapResult = VesperaDirectBufferPool.dispatchHeap(
                new byte[] {1}, ignored -> new byte[] {2, 3});
        byte[] bytes = new byte[heapResult.remaining()];
        heapResult.get(bytes);
        assertArrayEquals(new byte[] {2, 3}, bytes);
        assertTrue(heapResult.isReadOnly());
    }

    @Test
    void pooledHeapFallbacksReturnInjectedDispatchBytesForEveryRequestShape() {
        byte[] response = new byte[] {8, 6, 7};
        ByteBuffer raw = VesperaDirectBufferPool.dispatchDirectPooled(
                new byte[] {1}, false, true, request -> response);
        assertReadOnlyBytes(response, raw);

        String previous = System.getProperty("vespera.direct.oversize-policy");
        try {
            System.setProperty("vespera.direct.oversize-policy", "heap-fallback");
            ByteBuffer mapped = VesperaDirectBufferPool.dispatchDirectPooled(
                    null, "GET", "/map", null, Map.of(), null, false,
                    () -> true, request -> response);
            assertReadOnlyBytes(response, mapped);

            ByteBuffer sourced = VesperaDirectBufferPool.dispatchDirectPooled(
                    null, "GET", "/source", null,
                    (VesperaBridge.HeaderSource) sink -> sink.put("x", "y"),
                    null, false, true, request -> response);
            assertReadOnlyBytes(response, sourced);

            byte[] oversizedRaw = new byte[4 * 1024 * 1024 + 1];
            ByteBuffer rawOversized = VesperaDirectBufferPool.dispatchDirectPooled(
                    oversizedRaw, false, false, request -> {
                        assertSame(oversizedRaw, request);
                        return response;
                    });
            assertReadOnlyBytes(response, rawOversized);

            byte[] oversizedBody = new byte[4 * 1024 * 1024 + 1];
            ByteBuffer sourcedOversized = VesperaDirectBufferPool.dispatchDirectPooled(
                    null, "POST", "/source-large", null,
                    (VesperaBridge.HeaderSource) sink -> sink.put("x", "y"),
                    oversizedBody, false, false, request -> {
                        assertTrue(request.length > oversizedBody.length);
                        return response;
                    });
            assertReadOnlyBytes(response, sourcedOversized);
        } finally {
            restoreProperty("vespera.direct.oversize-policy", previous);
        }
    }

    @Test
    void directReturnCodeSeamCoversSuccessAndEveryOverflowOutcome() {
        ByteBuffer[] successPool = freshPool();
        ByteBuffer success = VesperaDirectBufferPool.dispatchViaPool(
                successPool, 12, false, (in, inLen, out) -> {
                    out.put(0, (byte) 4);
                    out.put(1, (byte) 5);
                    return 2;
                });
        byte[] successBytes = new byte[success.remaining()];
        success.get(successBytes);
        assertArrayEquals(new byte[] {4, 5}, successBytes);
        assertTrue(success.isReadOnly());

        IllegalStateException unrepresentable = assertThrows(
                IllegalStateException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, false,
                        (in, inLen, out) -> Integer.MIN_VALUE));
        assertEquals(
                "dispatchDirect response exceeds 2 GiB and cannot be represented; use streaming dispatch",
                unrepresentable.getMessage());

        VesperaBridge.BufferTooSmallException noRetry = assertThrows(
                VesperaBridge.BufferTooSmallException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, false, (in, inLen, out) -> -70_000));
        assertEquals(70_000, noRetry.requiredSize());

        VesperaBridge.BufferTooSmallException abovePoolCap = assertThrows(
                VesperaBridge.BufferTooSmallException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, true, (in, inLen, out) -> -(257 * 1024 * 1024)));
        assertEquals(257 * 1024 * 1024, abovePoolCap.requiredSize());

        AtomicInteger calls = new AtomicInteger();
        ByteBuffer[] retryPool = freshPool();
        ByteBuffer retry = VesperaDirectBufferPool.dispatchViaPool(
                retryPool, 1, true, (in, inLen, out) -> {
                    if (calls.getAndIncrement() == 0) return -70_000;
                    out.put(0, (byte) 9);
                    return 1;
                });
        assertEquals(2, calls.get());
        assertEquals(9, retry.get(0));
        assertEquals(128 * 1024, retryPool[1].capacity());

        AtomicInteger secondMinCalls = new AtomicInteger();
        assertThrows(IllegalStateException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, true,
                        (in, inLen, out) -> secondMinCalls.getAndIncrement() == 0
                                ? -70_000 : Integer.MIN_VALUE));
        assertEquals(2, secondMinCalls.get());

        AtomicInteger secondOverflowCalls = new AtomicInteger();
        VesperaBridge.BufferTooSmallException secondOverflow = assertThrows(
                VesperaBridge.BufferTooSmallException.class,
                () -> VesperaDirectBufferPool.dispatchViaPool(
                        freshPool(), 1, true,
                        (in, inLen, out) -> secondOverflowCalls.getAndIncrement() == 0
                                ? -70_000 : -90_000));
        assertEquals(90_000, secondOverflow.requiredSize());
        assertEquals(2, secondOverflowCalls.get());
    }

    private static ByteBuffer[] freshPool() {
        return new ByteBuffer[] {
                ByteBuffer.allocateDirect(64 * 1024),
                ByteBuffer.allocateDirect(64 * 1024)};
    }

    private static void assertReadOnlyBytes(byte[] expected, ByteBuffer actual) {
        byte[] bytes = new byte[actual.remaining()];
        actual.get(bytes);
        assertArrayEquals(expected, bytes);
        assertTrue(actual.isReadOnly());
    }

    private static boolean throwRuntime(Thread ignored) {
        throw new IllegalStateException("runtime");
    }

    private static boolean throwChecked(Thread ignored) throws java.io.IOException {
        throw new java.io.IOException("checked");
    }

    private static void restoreProperty(String name, String value) {
        if (value == null) {
            System.clearProperty(name);
        } else {
            System.setProperty(name, value);
        }
    }
}
