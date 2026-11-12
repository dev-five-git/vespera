package com.devfive.vespera.bridge;

import org.slf4j.Logger;

import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Per-request observation state of {@link VesperaProxyController}: filled in
 * as the request is resolved, then {@link #complete completed} once —
 * logging panics / 5xx at WARN and notifying every {@link DispatchObserver}.
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class ProxyExchange {
    private final Logger log;
    private final List<DispatchObserver> observers;
    private final long startNanos = System.nanoTime();
    final String method;
    final String requestId;
    String app;
    DispatchMode mode;
    String path;
    private boolean completed;

    ProxyExchange(Logger log, List<DispatchObserver> observers, String method, String requestId) {
        this.log = log;
        this.observers = observers;
        this.method = method;
        this.requestId = requestId;
    }

    /** Report the outcome; only the first call counts. */
    void complete(int status, boolean panicked) {
        if (completed) {
            return;
        }
        completed = true;
        long durationNanos = System.nanoTime() - startNanos;
        String appName = app == null ? AppNameResolver.DEFAULT_APP : app;
        if (panicked || status >= 500) {
            log.warn("vespera {} {} {} app={} mode={} status={} latency={}ms requestId={}",
                    panicked ? "Rust panic on" : "server error on",
                    method, path, appName, mode, status,
                    TimeUnit.NANOSECONDS.toMillis(durationNanos), requestId);
        }
        if (observers.isEmpty()) {
            return;
        }
        DispatchEvent event = new DispatchEvent(
                appName, mode, method, path, status, durationNanos, panicked, requestId);
        for (DispatchObserver observer : observers) {
            try {
                observer.onComplete(event);
            } catch (RuntimeException failure) {
                log.warn("DispatchObserver {} failed", observer, failure);
            }
        }
    }
}
