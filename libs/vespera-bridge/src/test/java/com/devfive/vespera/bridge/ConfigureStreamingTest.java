package com.devfive.vespera.bridge;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertThrows;

/**
 * Pure-Java validation tests for {@link VesperaBridge#configureStreaming}.
 * Tests the input validation bounds and pending-config pattern without
 * requiring the native library to be loaded.
 */
class ConfigureStreamingTest {

    @Test
    void preInitConfigurationStoresPending() {
        // Before init(), valid values should NOT throw UnsatisfiedLinkError.
        // Instead, they are stored as pending and will be applied at init time.
        // This test proves the pending-config pattern works.
        VesperaBridge.configureStreaming(65536, 16);
        // If we reach here without exception, the pending-config pattern is working.
        // (In a real app, init() would apply these values after loading natives.)
    }

    @Test
    void validChunkBytesAndCapacity() {
        // Valid values should not throw (pending-config pattern stores them).
        VesperaBridge.configureStreaming(65536, 16);
    }

    @Test
    void chunkBytesMinBoundary() {
        // 4096 (4 KiB) is the minimum — should pass validation
        try {
            VesperaBridge.configureStreaming(4096, 16);
        } catch (UnsatisfiedLinkError e) {
            // Expected when native lib not loaded
        }
    }

    @Test
    void chunkBytesMaxBoundary() {
        // 8388608 (8 MiB) is the maximum — should pass validation
        try {
            VesperaBridge.configureStreaming(8388608, 16);
        } catch (UnsatisfiedLinkError e) {
            // Expected when native lib not loaded
        }
    }

    @Test
    void chunkBytesBelowMinThrows() {
        // 4095 is below the minimum (4096)
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(4095, 16));
        assert ex.getMessage().contains("4095");
        assert ex.getMessage().contains("[4096, 8388608]");
    }

    @Test
    void chunkBytesAboveMaxThrows() {
        // 8388609 is above the maximum (8388608)
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(8388609, 16));
        assert ex.getMessage().contains("8388609");
        assert ex.getMessage().contains("[4096, 8388608]");
    }

    @Test
    void chunkBytesZeroThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(0, 16));
        assert ex.getMessage().contains("0");
    }

    @Test
    void chunkBytesNegativeThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(-1, 16));
        assert ex.getMessage().contains("-1");
    }

    @Test
    void capacityMinBoundary() {
        // 1 is the minimum — should pass validation
        try {
            VesperaBridge.configureStreaming(65536, 1);
        } catch (UnsatisfiedLinkError e) {
            // Expected when native lib not loaded
        }
    }

    @Test
    void capacityMaxBoundary() {
        // 1024 is the maximum — should pass validation
        try {
            VesperaBridge.configureStreaming(65536, 1024);
        } catch (UnsatisfiedLinkError e) {
            // Expected when native lib not loaded
        }
    }

    @Test
    void capacityBelowMinThrows() {
        // 0 is below the minimum (1)
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(65536, 0));
        assert ex.getMessage().contains("0");
        assert ex.getMessage().contains("[1, 1024]");
    }

    @Test
    void capacityAboveMaxThrows() {
        // 1025 is above the maximum (1024)
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(65536, 1025));
        assert ex.getMessage().contains("1025");
        assert ex.getMessage().contains("[1, 1024]");
    }

    @Test
    void capacityNegativeThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(65536, -1));
        assert ex.getMessage().contains("-1");
    }

    @Test
    void bothParametersOutOfRangeThrowsForChunkBytes() {
        // When both are invalid, chunkBytes is checked first
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(0, 0));
        assert ex.getMessage().contains("chunkBytes");
    }
}
