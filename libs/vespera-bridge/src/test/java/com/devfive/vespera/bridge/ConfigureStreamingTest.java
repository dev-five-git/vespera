package com.devfive.vespera.bridge;

import java.lang.reflect.Field;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ConfigureStreamingTest {

    @Test
    void preInitConfigurationStoresPending() {
        assertDoesNotThrow(() -> VesperaBridge.configureStreaming(65536, 16));
    }

    @Test
    void validChunkBytesAndCapacity() {
        assertDoesNotThrow(() -> VesperaBridge.configureStreaming(65536, 16));
    }

    @Test
    void chunkBytesMinBoundary() {
        assertDoesNotThrow(() -> VesperaBridge.configureStreaming(4096, 16));
    }

    @Test
    void chunkBytesMaxBoundary() {
        assertDoesNotThrow(() -> VesperaBridge.configureStreaming(8388608, 16));
    }

    @Test
    void chunkBytesBelowMinThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(4095, 16));
        assertTrue(ex.getMessage().contains("4095"));
        assertTrue(ex.getMessage().contains("[4096, 8388608]"));
    }

    @Test
    void chunkBytesAboveMaxThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(8388609, 16));
        assertTrue(ex.getMessage().contains("8388609"));
        assertTrue(ex.getMessage().contains("[4096, 8388608]"));
    }

    @Test
    void chunkBytesZeroThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(0, 16));
        assertTrue(ex.getMessage().contains("0"));
    }

    @Test
    void chunkBytesNegativeThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(-1, 16));
        assertTrue(ex.getMessage().contains("-1"));
    }

    @Test
    void capacityMinBoundary() {
        assertDoesNotThrow(() -> VesperaBridge.configureStreaming(65536, 1));
    }

    @Test
    void capacityMaxBoundary() {
        assertDoesNotThrow(() -> VesperaBridge.configureStreaming(65536, 1024));
    }

    @Test
    void capacityBelowMinThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(65536, 0));
        assertTrue(ex.getMessage().contains("0"));
        assertTrue(ex.getMessage().contains("[1, 1024]"));
    }

    @Test
    void capacityAboveMaxThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(65536, 1025));
        assertTrue(ex.getMessage().contains("1025"));
        assertTrue(ex.getMessage().contains("[1, 1024]"));
    }

    @Test
    void capacityNegativeThrows() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(65536, -1));
        assertTrue(ex.getMessage().contains("-1"));
    }

    @Test
    void bothParametersOutOfRangeThrowsForChunkBytes() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(0, 0));
        assertTrue(ex.getMessage().contains("chunkBytes"));
    }

    @Test
    void postInitMissingOptionalNativeHookDoesNotThrowRawLinkageError() throws Exception {
        Field loadedField = VesperaBridge.class.getDeclaredField("loaded");
        Field nameField = VesperaBridge.class.getDeclaredField("loadedLibraryName");
        loadedField.setAccessible(true);
        nameField.setAccessible(true);
        boolean prevLoaded = loadedField.getBoolean(null);
        Object prevName = nameField.get(null);
        try {
            loadedField.setBoolean(null, true);
            nameField.set(null, "older-native-without-configure-streaming");

            assertDoesNotThrow(() -> VesperaBridge.configureStreaming(65536, 16));
        } finally {
            loadedField.setBoolean(null, prevLoaded);
            nameField.set(null, prevName);
        }
    }
}
