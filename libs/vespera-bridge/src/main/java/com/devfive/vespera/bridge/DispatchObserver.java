package com.devfive.vespera.bridge;

/**
 * Observability SPI of the autoconfigured {@link VesperaProxyController}:
 * every {@code DispatchObserver} bean is invoked once per proxied request,
 * after its response was produced (on the request thread, or on the async
 * response executor for {@link DispatchMode#ASYNC}).
 *
 * <p>Keep implementations fast and non-blocking; an exception thrown here is
 * logged and swallowed, never surfaced to the client. When a Micrometer
 * {@code MeterRegistry} bean exists, {@link MicrometerDispatchObserver} is
 * registered automatically.
 */
@FunctionalInterface
public interface DispatchObserver {
    void onComplete(DispatchEvent event);
}
