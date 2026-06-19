package com.devfive.vespera.bridge;

import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;

import static org.junit.jupiter.api.Assertions.assertEquals;
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
    void directPoolClearsThreadLocalAfterIdleStreak() {
        ByteBuffer[] pool = VesperaDirectBufferPool.directPoolForTest();

        for (int i = 0; i < 8; i++) {
            VesperaDirectBufferPool.recordDirectPoolUseForTest(pool, 1, 1);
        }

        assertTrue(!VesperaDirectBufferPool.directPoolPresentForTest());
    }
}
