package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.lang.reflect.Field;
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
}
