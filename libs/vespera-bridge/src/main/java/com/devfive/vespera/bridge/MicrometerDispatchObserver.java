package com.devfive.vespera.bridge;

import io.micrometer.core.instrument.MeterRegistry;
import io.micrometer.core.instrument.Timer;

import java.util.Objects;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

/**
 * {@link DispatchObserver} recording the Micrometer timer
 * {@value #METER_NAME}, tagged {@code app}, {@code mode}, {@code status} and
 * {@code outcome} ({@code INFORMATIONAL} / {@code SUCCESS} /
 * {@code REDIRECTION} / {@code CLIENT_ERROR} / {@code SERVER_ERROR}, or
 * {@code PANIC} for a caught Rust panic). Autoconfigured when a
 * {@link MeterRegistry} bean exists.
 *
 * <p>Tag cardinality is bounded: the {@code app} tag is the
 * {@link DispatchEvent#app()} the proxy reports, which is a known app or
 * {@code UNKNOWN} — never the raw client-selected name — and {@code status}
 * is clamped to {@code 100..599} ({@code OTHER} outside). Timers are cached per
 * tag combination instead of being looked up through a builder per request.
 */
public class MicrometerDispatchObserver implements DispatchObserver {
    public static final String METER_NAME = "vespera.bridge.dispatch";

    /** Hard cap on cached timers; beyond it a timer is looked up per event (never unbounded memory). */
    static final int MAX_CACHED_TIMERS = 4096;

    private final MeterRegistry registry;
    private final ConcurrentHashMap<Key, Timer> timers = new ConcurrentHashMap<>();

    private record Key(String app, DispatchMode mode, int status, boolean panicked) {}

    public MicrometerDispatchObserver(MeterRegistry registry) {
        this.registry = Objects.requireNonNull(registry, "registry");
    }

    @Override
    public void onComplete(DispatchEvent event) {
        Key key = new Key(event.app(), event.mode(), event.status(), event.panicked());
        Timer timer = timers.get(key);
        if (timer == null) {
            timer = register(key);
            if (timers.size() < MAX_CACHED_TIMERS) {
                timers.putIfAbsent(key, timer);
            }
        }
        timer.record(event.durationNanos(), TimeUnit.NANOSECONDS);
    }

    private Timer register(Key key) {
        return Timer.builder(METER_NAME)
                .description("Requests proxied to the Rust vespera engine")
                .tag("app", key.app() == null ? AppNameResolver.DEFAULT_APP : key.app())
                .tag("mode", key.mode() == null ? "NONE" : key.mode().name())
                .tag("status", statusTag(key.status()))
                .tag("outcome", outcome(key.status(), key.panicked()))
                .register(registry);
    }

    static String statusTag(int status) {
        return status >= 100 && status <= 599 ? Integer.toString(status) : "OTHER";
    }

    static String outcome(int status, boolean panicked) {
        if (panicked) {
            return "PANIC";
        }
        if (status >= 500) {
            return "SERVER_ERROR";
        }
        if (status >= 400) {
            return "CLIENT_ERROR";
        }
        if (status >= 300) {
            return "REDIRECTION";
        }
        return status >= 200 ? "SUCCESS" : "INFORMATIONAL";
    }
}
