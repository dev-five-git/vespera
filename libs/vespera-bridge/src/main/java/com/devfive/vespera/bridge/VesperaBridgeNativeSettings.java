package com.devfive.vespera.bridge;

/**
 * Marker bean proving {@code vespera.bridge.native.*} has been forwarded to
 * the native side (and, with {@code vespera.bridge.library-name}, that the
 * library was initialised).  The autoconfigured proxy (servlet controller or
 * reactive handler) depends on it so the settings are applied before the
 * first request.
 */
public final class VesperaBridgeNativeSettings {
    private final boolean autoInitialized;

    public VesperaBridgeNativeSettings() {
        this(false);
    }

    public VesperaBridgeNativeSettings(boolean autoInitialized) {
        this.autoInitialized = autoInitialized;
    }

    /** Whether the autoconfiguration itself called {@link VesperaBridge#init(String)}. */
    public boolean autoInitialized() {
        return autoInitialized;
    }

    /**
     * Forward {@code vespera.bridge.native.*} to the native side and, with
     * {@code vespera.bridge.library-name}, initialise the library — shared by
     * the servlet and reactive autoconfigurations.
     */
    static VesperaBridgeNativeSettings apply(VesperaBridgeProperties props) {
        props.getNative().apply(props.getMaxBufferedResponseBytes());
        return new VesperaBridgeNativeSettings(autoInit(props.getLibraryName()));
    }

    /**
     * {@code vespera.bridge.library-name}: initialise the native library
     * unless the application already did. Returns whether this call loaded it.
     */
    static boolean autoInit(String libraryName) {
        if (libraryName == null || libraryName.isBlank()) {
            return false;
        }
        synchronized (VesperaBridge.class) {
            if (VesperaBridge.isInitialized()) {
                return false;
            }
            VesperaBridge.init(libraryName.trim());
            return true;
        }
    }
}
