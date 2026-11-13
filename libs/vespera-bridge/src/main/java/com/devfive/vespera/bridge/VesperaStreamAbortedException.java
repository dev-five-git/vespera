package com.devfive.vespera.bridge;

import java.io.IOException;

/**
 * Thrown by the streaming dispatch natives ({@code dispatchStreaming},
 * {@code dispatchFullStreaming}, the {@code *WithHeader} variants and the
 * stashed DIRECT overflow stream) when the response failed <em>after</em> its
 * header was delivered: the body errored, the output sink stopped, the
 * header consumer threw, or a Rust handler / body stream panicked.  The body
 * written so far is truncated, so the caller must not complete the response
 * as if it were whole: answer an error while the response is still
 * uncommitted, otherwise abort the transport.
 *
 * <p>{@link #panicked()} tells a caught Rust panic apart from the other
 * causes (the streaming counterpart of the {@code "panicked":"true"} wire
 * metadata).  A cdylib older than this class, or a native image whose JNI
 * metadata does not register it, throws a plain {@link IOException} with the
 * same message instead.
 */
public final class VesperaStreamAbortedException extends IOException {
    private static final long serialVersionUID = 1L;

    /**
     * Message the native side uses for a panic after the response header was
     * produced (kept in sync with {@code AbortReason::Panicked} in
     * {@code crates/vespera_jni/src/jni_impl_support.rs}).
     */
    public static final String PANIC_MESSAGE =
            "vespera: panic in Rust engine after the response header was produced";

    private final boolean panicked;

    /** Called from native code with one of the abort messages. */
    public VesperaStreamAbortedException(String message) {
        super(message);
        this.panicked = PANIC_MESSAGE.equals(message);
    }

    /** {@code true} when the abort was caused by a caught Rust panic. */
    public boolean panicked() {
        return panicked;
    }

    /**
     * Whether {@code failure} reports a Rust panic: a
     * {@link VesperaStreamAbortedException} flagged {@link #panicked()}, or a
     * plain {@link IOException} carrying {@link #PANIC_MESSAGE} (the fallback
     * when the typed class could not be resolved natively).
     */
    static boolean isPanic(IOException failure) {
        return failure instanceof VesperaStreamAbortedException aborted
                ? aborted.panicked()
                : PANIC_MESSAGE.equals(failure.getMessage());
    }
}
