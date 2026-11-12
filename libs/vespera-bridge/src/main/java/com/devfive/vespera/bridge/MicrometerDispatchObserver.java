package com.devfive.vespera.bridge;

import io.micrometer.core.instrument.MeterRegistry;
import io.micrometer.core.instrument.Timer;

import java.util.Objects;
import java.util.concurrent.TimeUnit;

/**
 * {@link DispatchObserver} recording the Micrometer timer
 * {@value #METER_NAME}, tagged {@code app}, {@code mode}, {@code status} and
 * {@code outcome} ({@code INFORMATIONAL} / {@code SUCCESS} /
 * {@code REDIRECTION} / {@code CLIENT_ERROR} / {@code SERVER_ERROR}, or
 * {@code PANIC} for a caught Rust panic). Autoconfigured when a
 * {@link MeterRegistry} bean exists.
 */
public class MicrometerDispatchObserver implements DispatchObserver {
    public static final String METER_NAME = "vespera.bridge.dispatch";

    private final MeterRegistry registry;

    public MicrometerDispatchObserver(MeterRegistry registry) {
        this.registry = Objects.requireNonNull(registry, "registry");
    }

    @Override
    public void onComplete(DispatchEvent event) {
        Timer.builder(METER_NAME)
                .description("Requests proxied to the Rust vespera engine")
                .tag("app", event.app())
                .tag("mode", event.mode() == null ? "NONE" : event.mode().name())
                .tag("status", Integer.toString(event.status()))
                .tag("outcome", outcome(event.status(), event.panicked()))
                .register(registry)
                .record(event.durationNanos(), TimeUnit.NANOSECONDS);
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
