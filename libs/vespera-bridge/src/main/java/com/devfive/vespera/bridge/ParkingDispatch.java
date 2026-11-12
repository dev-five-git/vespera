package com.devfive.vespera.bridge;

/**
 * Buffered dispatch that does not pin a virtual thread's carrier.
 *
 * <p>{@link VesperaBridge#dispatchBytes(byte[])} blocks inside a native
 * frame for the whole handler run; a virtual thread blocked in native code
 * stays <em>pinned</em> to its carrier, so a few slow handlers can starve
 * every other virtual thread of carriers.  On a virtual thread the same
 * request instead goes through {@link VesperaBridge#dispatchAsync} and
 * {@code join()}: the native call returns right after scheduling, and the
 * virtual thread <em>parks</em> (carrier released) until a Tokio worker
 * completes the future.  Costs one {@code CompletableFuture} and a
 * worker-thread completion (~1.5&nbsp;&micro;s measured for the async path),
 * and the {@code vespera.async.maxInFlight} cap applies ({@code 503} beyond
 * it).  Platform threads keep the direct {@code dispatchBytes} call.
 *
 * <p>Streaming modes cannot take this route — their native calls write to
 * the servlet streams from the calling thread — so they still pin.
 */
final class ParkingDispatch {

    private ParkingDispatch() {}

    /** {@code dispatchBytes}, parking instead of pinning on a virtual thread. */
    static byte[] dispatchBytes(byte[] wireRequest, boolean currentThreadIsVirtual) {
        return currentThreadIsVirtual
                ? park(wireRequest)
                : VesperaBridge.dispatchBytes(wireRequest);
    }

    /** Async dispatch + {@code join()} (the future always completes with a wire). */
    static byte[] park(byte[] wireRequest) {
        return VesperaBridge.dispatch(wireRequest).join();
    }
}
