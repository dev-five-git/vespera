package com.devfive.vespera.bridge;

import java.util.concurrent.CompletableFuture;

/**
 * Buffered dispatch that does not pin a virtual thread's carrier.
 *
 * <p>{@link VesperaBridge#dispatchBytes(byte[])} blocks inside a native
 * frame for the whole handler run; a virtual thread blocked in native code
 * stays <em>pinned</em> to its carrier, so a few slow handlers can starve
 * every other virtual thread of carriers.  On a virtual thread the same
 * request therefore never blocks natively across a wait:
 *
 * <ul>
 *   <li><strong>poll-first</strong> (default): the native side polls the
 *       dispatch once on the calling thread.  A handler that completes
 *       without waiting returns its response right there (pinned-call cost,
 *       the carrier only did CPU work); one that has to wait is handed,
 *       half-polled, to a Tokio worker and the virtual thread <em>parks</em>
 *       on the {@code CompletableFuture} that worker completes.</li>
 *   <li>{@code -Dvespera.virtualThread.pollFirst=false}: every request goes
 *       through {@link VesperaBridge#dispatchAsync} + {@code join()} (the
 *       pre-0.6 behaviour) &mdash; measured ~10&times; the latency of a
 *       pinned call for a handler that never waits.</li>
 * </ul>
 *
 * <p>Both apply the {@code vespera.async.maxInFlight} cap ({@code 503} beyond
 * it).  Platform threads keep the direct {@code dispatchBytes} call.
 * Streaming modes cannot take this route &mdash; their native calls write to
 * the servlet streams from the calling thread &mdash; so they still pin.
 * Numbers: {@code docs/virtual-thread-park.md}.
 */
final class ParkingDispatch {

    /** System property switching the poll-first strategy off ({@code false}). */
    static final String POLL_FIRST_PROPERTY = "vespera.virtualThread.pollFirst";

    private static final boolean POLL_FIRST = pollFirstEnabled(System.getProperty(POLL_FIRST_PROPERTY));

    /** Native seam: wire when done on the calling thread, {@code null} when {@code future} will be completed. */
    @FunctionalInterface
    interface PollFirstDispatcher {
        byte[] dispatch(CompletableFuture<byte[]> future, byte[] wireRequest);
    }

    private ParkingDispatch() {}

    static boolean pollFirstEnabled(String configured) {
        return configured == null || !"false".equalsIgnoreCase(configured.trim());
    }

    /** {@code dispatchBytes}, parking instead of pinning on a virtual thread. */
    static byte[] dispatchBytes(byte[] wireRequest, boolean currentThreadIsVirtual) {
        return currentThreadIsVirtual
                ? park(wireRequest)
                : VesperaBridge.dispatchBytes(wireRequest);
    }

    /** Dispatch without holding the carrier across a wait (the future always completes with a wire). */
    static byte[] park(byte[] wireRequest) {
        return park(wireRequest, POLL_FIRST, AsyncDispatch::dispatchPollFirst0);
    }

    static byte[] park(byte[] wireRequest, boolean pollFirst, PollFirstDispatcher pollFirstDispatcher) {
        if (!pollFirst) {
            return VesperaBridge.dispatch(wireRequest).join();
        }
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        byte[] done = pollFirstDispatcher.dispatch(future, wireRequest);
        return done != null ? done : future.join();
    }
}
