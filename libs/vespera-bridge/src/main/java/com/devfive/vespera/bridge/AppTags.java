package com.devfive.vespera.bridge;

import java.util.Collection;
import java.util.HashSet;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Bounded app names for metrics, logs and {@link DispatchEvent}s.
 *
 * <p>The app a request selects is client-controlled (the
 * {@code X-Vespera-App} header), so reporting it verbatim lets any client mint
 * unbounded metric series ({@code app} tag) and log values.  This class maps a
 * selected name to itself only when it is known to be a real app — the default
 * app, an app listed in {@code vespera.bridge.exposed-apps}, or an app Rust has
 * proven registered by answering a request for it from one of its handlers —
 * and to {@link #UNKNOWN} otherwise (names rejected by the allowlist, invalid
 * names Rust answers {@code 400}, unregistered names Rust answers {@code 404}).
 *
 * <p>"Answered from a handler" means the request reached Rust and produced a
 * status Rust's app resolution never returns: below {@code 400}, a
 * {@code 4xx} other than {@code 400 / 404 / 408 / 413 / 431}, a {@code 5xx}
 * other than {@code 503}, or a caught panic.  {@code 503} is excluded because
 * Rust answers it before app lookup too (shut-down runtime, async in-flight
 * cap), and the wire cannot tell those from a handler's own {@code 503}; a
 * handler app that only ever answers {@code 503} therefore stays
 * {@link #UNKNOWN}.  The learned set therefore only ever holds registered app
 * names; it is additionally capped at {@value #MAX_LEARNED} entries.
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class AppTags {
    /** Tag of an app name that is not known to be registered. */
    static final String UNKNOWN = "UNKNOWN";

    static final int MAX_LEARNED = 256;

    private final Set<String> preset;
    private final Set<String> learned = ConcurrentHashMap.newKeySet();
    /** Slots reserved in {@link #learned}; reserved before the add so the cap holds under races. */
    private final AtomicInteger learnedCount = new AtomicInteger();

    AppTags(Collection<String> allowlisted) {
        Set<String> names = new HashSet<>();
        names.add(AppNameResolver.DEFAULT_APP);
        if (allowlisted != null) {
            names.addAll(allowlisted);
        }
        this.preset = Set.copyOf(names);
    }

    /** Tags seeded from the resolver's allowlist when it is a restricted {@link HeaderAppNameResolver}. */
    static AppTags forResolver(AppNameResolver resolver) {
        return new AppTags(resolver instanceof HeaderAppNameResolver header ? header.exposedApps() : null);
    }

    /**
     * The bounded name for {@code app} ({@code null} = default app).
     *
     * @param reachedRust whether the request was handed to the native dispatch
     */
    String tag(String app, int status, boolean panicked, boolean reachedRust) {
        if (app == null) {
            return AppNameResolver.DEFAULT_APP;
        }
        if (preset.contains(app) || learned.contains(app)) {
            return app;
        }
        if (reachedRust && answeredByHandler(status, panicked) && learn(app)) {
            return app;
        }
        return UNKNOWN;
    }

    /**
     * Learns {@code app} unless the cap is full. A slot is reserved first and
     * released again when the cap is already reached or another thread
     * learned the same name, so concurrent callers cannot overshoot
     * {@link #MAX_LEARNED}.
     */
    private boolean learn(String app) {
        if (learnedCount.incrementAndGet() > MAX_LEARNED) {
            learnedCount.decrementAndGet();
            return false;
        }
        if (learned.add(app)) {
            return true;
        }
        learnedCount.decrementAndGet();
        return true;
    }

    /**
     * Whether a status proves the app was resolved and its handler ran (or
     * panicked). The statuses Rust answers before app lookup are excluded:
     * {@code 400}, {@code 404}, {@code 408}, {@code 413}, {@code 431} and
     * {@code 503}.
     */
    static boolean answeredByHandler(int status, boolean panicked) {
        if (panicked) {
            return true;
        }
        if (status >= 100 && status < 400) {
            return true;
        }
        if (status >= 400 && status < 500) {
            return status != 400 && status != 404 && status != 408 && status != 413
                    && status != 431;
        }
        return status >= 500 && status < 600 && status != 503;
    }
}
