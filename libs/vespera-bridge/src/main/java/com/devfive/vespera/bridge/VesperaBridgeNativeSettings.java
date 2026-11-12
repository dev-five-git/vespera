package com.devfive.vespera.bridge;

/**
 * Marker bean proving {@code vespera.bridge.native.*} has been forwarded to
 * the native side (and, with {@code vespera.bridge.library-name}, that the
 * library was initialised).  The autoconfigured proxy controller depends on
 * it so the settings are applied before the first request.
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
}
