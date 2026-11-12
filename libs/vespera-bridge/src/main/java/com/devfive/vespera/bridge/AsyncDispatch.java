package com.devfive.vespera.bridge;

import java.util.concurrent.CompletableFuture;
import java.util.function.LongPredicate;

/**
 * Natives and cancellation wiring behind
 * {@link VesperaBridge#dispatchAsync(CompletableFuture, byte[])}.
 *
 * <p>{@link #dispatch0} spawns the dispatch on the shared Tokio runtime and
 * returns a non-zero <em>cancellation token</em> (or {@code 0} when the future
 * was already completed on the calling thread: in-flight cap reached, invalid
 * input, scheduling failure).  {@link #dispatch} then links the future to
 * {@link #cancel0}: when the future completes <strong>exceptionally</strong>
 * &mdash; {@code cancel(..)}, {@code orTimeout(..)}, an external
 * {@code completeExceptionally(..)} &mdash; nobody can observe the native
 * result any more, so the Rust task is aborted: the handler future is dropped
 * at its next poll and its {@code vespera.async.maxInFlight} slot is freed.
 * The native side never completes the future exceptionally (always a wire
 * response), so a normal completion never triggers a cancel; a cancel racing
 * the normal completion is harmless (an unknown token is a no-op).
 */
final class AsyncDispatch {

    private AsyncDispatch() {}

    /** Dispatch {@code wireRequest} and propagate cancellation of {@code future} to Rust. */
    static void dispatch(CompletableFuture<byte[]> future, byte[] wireRequest) {
        long token = dispatch0(future, wireRequest);
        linkCancellation(future, token, AsyncDispatch::cancel0);
    }

    /**
     * Abort native task {@code token} via {@code canceller} once {@code future}
     * completes exceptionally (also when that already happened between the
     * native spawn and this call).  No-op for token {@code 0} (nothing was
     * spawned) or a normally completed future (the native result arrived).
     */
    static void linkCancellation(CompletableFuture<?> future, long token, LongPredicate canceller) {
        if (token == 0) {
            return;
        }
        if (future.isDone()) {
            // Fast path, no dependent stage: the common sub-microsecond
            // completion lands here.
            if (future.isCompletedExceptionally()) {
                canceller.test(token);
            }
            return;
        }
        // Runs inline on whichever thread completes the future: the Tokio
        // worker on a normal completion (just the null check), or the
        // cancelling thread (one short native call).
        future.whenComplete((ignored, failure) -> {
            if (failure != null) {
                canceller.test(token);
            }
        });
    }

    /**
     * Spawn the async dispatch; completes {@code future} from a Tokio worker.
     *
     * @return cancellation token, or {@code 0} when {@code future} was already
     *         completed on the calling thread
     */
    private static native long dispatch0(CompletableFuture<byte[]> future, byte[] wireRequest);

    /**
     * Abort the async dispatch registered under {@code token}.
     *
     * @return {@code true} when a live dispatch was cancelled, {@code false}
     *         when it had already finished (or the token is unknown)
     */
    static native boolean cancel0(long token);

    /**
     * Poll the dispatch once on the calling thread; when it is still pending,
     * hand it to a Tokio worker that completes {@code future} (see
     * {@link ParkingDispatch}).
     *
     * @return the wire response when the dispatch finished on this thread
     *         ({@code future} untouched), or {@code null} when {@code future}
     *         will be completed
     */
    static native byte[] dispatchPollFirst0(CompletableFuture<byte[]> future, byte[] wireRequest);
}
