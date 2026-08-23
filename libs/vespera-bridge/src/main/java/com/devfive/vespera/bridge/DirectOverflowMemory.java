package com.devfive.vespera.bridge;

import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Remembers which {@code (app, method, path)} targets have overflowed
 * the pooled DIRECT response buffer, so the proxy can skip DIRECT and stream
 * those targets directly on subsequent requests.
 *
 * <p>Without this, a known-large (e.g. download) route routed to
 * {@link DispatchMode#DIRECT} pays the DIRECT-overflow-then-stream
 * <strong>double dispatch</strong> on <em>every</em> request: the Rust handler
 * runs once into the pooled direct buffer (overflows), then again through
 * response streaming. After the first overflow this memory downgrades the route
 * to {@link DispatchMode#STREAMING} up front, so later requests dispatch once.
 *
 * <p>Thread-safe and bounded. {@link #shouldAvoidDirect} is a single volatile
 * read until the first overflow is recorded, so apps that never overflow DIRECT
 * — the steady state — pay no per-request cost. When the entry cap is reached
 * the set is cleared wholesale (an approximate bound that needs no dependency);
 * a re-learn then costs at most one extra overflow per affected route.
 */
final class DirectOverflowMemory {

    static final int DEFAULT_MAX_ENTRIES = 1024;

    private final int maxEntries;
    private final Set<RouteKey> overflowed = ConcurrentHashMap.newKeySet();

    // Hot-path guard: a single volatile read. Stays false (zero lookups) until
    // the first overflow is recorded; once true it never resets, because an app
    // with oversized DIRECT responses pays the cheap contains() from then on.
    private volatile boolean hasEntries = false;

    DirectOverflowMemory() {
        this(DEFAULT_MAX_ENTRIES);
    }

    DirectOverflowMemory(int maxEntries) {
        this.maxEntries = Math.max(1, maxEntries);
    }

    /**
     * Whether a prior DIRECT dispatch of this route overflowed the pooled
     * buffer (and so should stream up front instead of re-attempting DIRECT).
     */
    boolean shouldAvoidDirect(String appName, String method, String path, String query) {
        if (!hasEntries) {
            return false;
        }
        return overflowed.contains(RouteKey.of(appName, method, path));
    }

    boolean shouldAvoidDirect(String method, String path) {
        return shouldAvoidDirect(null, method, path, null);
    }

    /** Record that this route overflowed DIRECT so future requests stream. */
    void recordOverflow(String appName, String method, String path, String query) {
        if (overflowed.size() >= maxEntries) {
            overflowed.clear();
        }
        overflowed.add(RouteKey.of(appName, method, path));
        hasEntries = true;
    }

    void recordOverflow(String method, String path) {
        recordOverflow(null, method, path, null);
    }

    int size() {
        return overflowed.size();
    }

    private record RouteKey(String appName, String method, String path, int hash) {
        static RouteKey of(String appName, String method, String path) {
            String normalizedApp = appName == null || appName.isBlank() ? "_default" : appName;
            return new RouteKey(normalizedApp, method, path,
                    31 * (31 * normalizedApp.hashCode() + method.hashCode()) + path.hashCode());
        }

        @Override
        public int hashCode() {
            return hash;
        }

        @Override
        public boolean equals(Object obj) {
            if (this == obj) {
                return true;
            }
            return obj instanceof RouteKey other
                    && appName.equals(other.appName)
                    && method.equals(other.method)
                    && path.equals(other.path);
        }
    }
}
