package com.devfive.vespera.bridge;

import java.io.OutputStream;
import java.util.function.Consumer;

/**
 * A DIRECT overflow whose finished response is still held on the Rust side
 * (see {@link DirectStash}): it can be streamed with {@link #streamTo} — the
 * handler is <strong>not</strong> re-run — or released with {@link #discard}.
 * Exactly one of the two must be called (an unredeemed entry expires after
 * 30 s; the native stash never evicts it earlier, see {@link DirectStash}).
 * Raised internally by the pooled DIRECT path when the response is
 * an event stream or exceeds the pooled buffer cap; public callers only see
 * the plain {@link VesperaBridge.BufferTooSmallException} supertype.
 */
final class StashedOverflowException extends VesperaBridge.BufferTooSmallException {
    private static final long serialVersionUID = 1L;

    private final transient long token;
    private final boolean eventStream;
    private final transient DirectStash.Ops ops;

    StashedOverflowException(int requiredSize, long token, boolean eventStream, DirectStash.Ops ops) {
        super(requiredSize, eventStream
                ? "event-stream response kept for streaming"
                : "response of at least " + requiredSize
                        + " bytes exceeds the pooled direct buffer; kept for streaming");
        this.token = token;
        this.eventStream = eventStream;
        this.ops = ops;
    }

    /**
     * Pure control flow (caught by {@link DirectModeDispatcher}, never
     * logged): skip capturing a Spring-deep stack, several µs per throw.
     */
    @Override
    public synchronized Throwable fillInStackTrace() {
        return this;
    }

    /** Whether the stashed response is {@code text/event-stream}. */
    boolean eventStream() {
        return eventStream;
    }

    /** Stream the stashed response (header consumer first, then the body). */
    void streamTo(Consumer<byte[]> headerConsumer, OutputStream out) {
        ops.stream(token, headerConsumer, out);
    }

    /** Release the stashed response. */
    void discard() {
        ops.discard(token);
    }
}
