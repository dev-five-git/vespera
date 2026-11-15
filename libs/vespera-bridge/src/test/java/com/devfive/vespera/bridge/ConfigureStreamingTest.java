package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.lang.reflect.Field;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.CsvSource;

/** {@link VesperaBridge#configureStreaming}: argument ranges and the pending (pre-init) value. */
class ConfigureStreamingTest {

    @BeforeEach
    void requireNoNativeLibrary() {
        Assumptions.assumeFalse(VesperaBridge.isInitialized(), "the pending path needs an unloaded library");
        VesperaNativeConfig.clearPendingStreamingForTest();
    }

    @AfterEach
    void clearPending() {
        VesperaNativeConfig.clearPendingStreamingForTest();
    }

    @ParameterizedTest
    @CsvSource({
        "65536, 16",
        "4096, 16",      // chunkBytes lower bound
        "8388608, 16",   // chunkBytes upper bound
        "65536, 1",      // channelCapacity lower bound
        "65536, 1024",   // channelCapacity upper bound
    })
    void inRangeValuesAreStoredPendingBeforeInit(int chunkBytes, int channelCapacity) {
        VesperaBridge.configureStreaming(chunkBytes, channelCapacity);
        assertArrayEquals(new int[] {chunkBytes, channelCapacity}, VesperaNativeConfig.pendingStreamingForTest());
    }

    @ParameterizedTest
    @CsvSource({
        "4095, 16, 'chunkBytes 4095 out of range [4096, 8388608]'",
        "8388609, 16, 'chunkBytes 8388609 out of range [4096, 8388608]'",
        "0, 16, 'chunkBytes 0 out of range [4096, 8388608]'",
        "-1, 16, 'chunkBytes -1 out of range [4096, 8388608]'",
        "65536, 0, 'channelCapacity 0 out of range [1, 1024]'",
        "65536, 1025, 'channelCapacity 1025 out of range [1, 1024]'",
        "65536, -1, 'channelCapacity -1 out of range [1, 1024]'",
        // Both out of range: chunkBytes is reported first.
        "0, 0, 'chunkBytes 0 out of range [4096, 8388608]'",
    })
    void outOfRangeValuesAreRejectedAndNothingIsStored(int chunkBytes, int channelCapacity, String message) {
        VesperaBridge.configureStreaming(65536, 16);
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.configureStreaming(chunkBytes, channelCapacity));
        assertTrue(ex.getMessage().startsWith(message), ex.getMessage());
        assertArrayEquals(new int[] {65536, 16}, VesperaNativeConfig.pendingStreamingForTest(),
                "the previous pending value is kept");
    }

    @Test
    void nothingIsPendingUntilConfigured() {
        assertNull(VesperaNativeConfig.pendingStreamingForTest());
    }

    @Test
    void postInitMissingOptionalNativeHookDoesNotThrowRawLinkageError() throws Exception {
        Field loadedField = VesperaBridge.class.getDeclaredField("nativeLoaded");
        Field nameField = VesperaBridge.class.getDeclaredField("loadedLibraryName");
        loadedField.setAccessible(true);
        nameField.setAccessible(true);
        boolean prevLoaded = loadedField.getBoolean(null);
        Object prevName = nameField.get(null);
        try {
            loadedField.setBoolean(null, true);
            nameField.set(null, "older-native-without-configure-streaming");

            assertDoesNotThrow(() -> VesperaBridge.configureStreaming(65536, 16));
            assertNull(VesperaNativeConfig.pendingStreamingForTest(), "applied (attempted) natively, not pending");
        } finally {
            loadedField.setBoolean(null, prevLoaded);
            nameField.set(null, prevName);
        }
    }
}
