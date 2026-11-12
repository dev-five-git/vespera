package com.devfive.vespera.bridge;

import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Spring-free health state of the native engine, adapted to Spring Boot
 * Actuator by {@link VesperaBridgeHealthIndicator} (Boot 4) /
 * {@link VesperaBridgeBoot3HealthIndicator} (Boot 3.5).
 *
 * <ul>
 *   <li>{@link Status#DOWN DOWN} — the native library is not loaded
 *       ({@link VesperaBridge#init(String)} never ran or failed): the proxy
 *       answers every request {@code 503};</li>
 *   <li>{@link Status#OUT_OF_SERVICE OUT_OF_SERVICE} — the runtime was stopped
 *       by {@link VesperaBridge#shutdown(java.time.Duration)} (irreversible);</li>
 *   <li>{@link Status#UP UP} otherwise.</li>
 * </ul>
 */
public final class VesperaBridgeHealth {

    /** Health status, named like Spring Boot's built-in statuses. */
    public enum Status { UP, DOWN, OUT_OF_SERVICE }

    private VesperaBridgeHealth() {}

    public static Status status() {
        if (!VesperaBridge.isInitialized()) {
            return Status.DOWN;
        }
        return VesperaBridge.isShutdown() ? Status.OUT_OF_SERVICE : Status.UP;
    }

    /** {@code bridgeVersion}, {@code nativeVersion} (when known) and a {@code reason} unless UP. */
    public static Map<String, Object> details() {
        Map<String, Object> details = new LinkedHashMap<>();
        String bridge = VesperaVersions.bridgeVersion();
        if (bridge != null) {
            details.put("bridgeVersion", bridge);
        }
        String nativeVersion = VesperaVersions.nativeVersion();
        if (nativeVersion != null) {
            details.put("nativeVersion", nativeVersion);
        }
        switch (status()) {
            case DOWN -> details.put("reason", "native library not initialized");
            case OUT_OF_SERVICE -> details.put("reason", "native runtime shut down");
            default -> { }
        }
        return details;
    }
}
