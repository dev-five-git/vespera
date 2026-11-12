package com.devfive.vespera.bridge;

import org.slf4j.Logger;
import org.slf4j.MDC;

import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.Supplier;

/**
 * Per-request observation state of {@link VesperaProxyController}: filled in
 * as the request is resolved, then {@link #complete completed} once —
 * logging panics / 5xx at WARN and notifying every {@link DispatchObserver}.
 *
 * <p>The app reported to logs and observers is bounded by {@link AppTags}:
 * the client-selected name only when it is a known app, else
 * {@link AppTags#UNKNOWN}.  Completion is thread-safe (the ASYNC path can race
 * a Spring async timeout against the native completion; the first wins) and
 * runs with the request id in the SLF4J MDC, whichever thread calls it.
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class ProxyExchange {
    private final Logger log;
    private final List<DispatchObserver> observers;
    private final AppTags appTags;
    private final long startNanos = System.nanoTime();
    final String method;
    final String requestId;
    /** Client-selected app ({@code null} = default); never reported verbatim. */
    String app;
    DispatchMode mode;
    String path;
    /** Whether the request was handed to the native dispatch. */
    volatile boolean reachedRust;
    private final AtomicBoolean completed = new AtomicBoolean();

    ProxyExchange(Logger log, List<DispatchObserver> observers, String method, String requestId) {
        this(log, observers, new AppTags(null), method, requestId);
    }

    ProxyExchange(Logger log, List<DispatchObserver> observers, AppTags appTags,
            String method, String requestId) {
        this.log = log;
        this.observers = observers;
        this.appTags = appTags;
        this.method = method;
        this.requestId = requestId;
    }

    /** Whether {@link #complete} already ran. */
    boolean isCompleted() {
        return completed.get();
    }

    /** Report the outcome; only the first call counts. */
    void complete(int status, boolean panicked) {
        if (!completed.compareAndSet(false, true)) {
            return;
        }
        long durationNanos = System.nanoTime() - startNanos;
        String appName = appTags.tag(app, status, panicked, reachedRust);
        withMdc(requestId, () -> {
            if (panicked || status >= 500) {
                log.warn("vespera {} {} {} app={} mode={} status={} latency={}ms requestId={}",
                        panicked ? "Rust panic on" : "server error on",
                        method, path, appName, mode, status,
                        TimeUnit.NANOSECONDS.toMillis(durationNanos), requestId);
            }
            if (observers.isEmpty()) {
                return null;
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
            return null;
        });
    }

    /**
     * Run {@code body} with {@code requestId} in the MDC (key
     * {@link VesperaProxyController#MDC_REQUEST_ID}), restoring the previous
     * value afterwards — for work on threads other than the request thread.
     */
    static <T> T withMdc(String requestId, Supplier<T> body) {
        if (requestId == null) {
            return body.get();
        }
        String previous = MDC.get(VesperaProxyController.MDC_REQUEST_ID);
        MDC.put(VesperaProxyController.MDC_REQUEST_ID, requestId);
        try {
            return body.get();
        } finally {
            if (previous == null) {
                MDC.remove(VesperaProxyController.MDC_REQUEST_ID);
            } else {
                MDC.put(VesperaProxyController.MDC_REQUEST_ID, previous);
            }
        }
    }
}
