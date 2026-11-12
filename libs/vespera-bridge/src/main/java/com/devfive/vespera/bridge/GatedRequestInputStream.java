package com.devfive.vespera.bridge;

import java.io.IOException;
import java.io.InputStream;
import java.util.Objects;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Request-body gate handed to the bidirectional streaming dispatch in place
 * of the raw servlet {@link InputStream}.
 *
 * <p>The Rust side pulls the request body from a blocking producer thread.
 * When the handler finishes without consuming the whole body, the dispatch
 * closes the source and joins the producer only for a bounded grace period;
 * a producer still parked in {@code read} is then <em>detached</em> and may
 * keep reading after {@code dispatchFullStreaming*} has returned.  Servlet
 * input streams are not thread-safe and, on a keep-alive connection, bytes
 * read after the exchange ended could belong to the next request.
 *
 * <p>The gate makes that safe from the Java side:
 * <ul>
 *   <li>it counts reads currently in flight on the delegate;</li>
 *   <li>{@link #detach()} (called once the native dispatch returned) and
 *       {@link #close()} close the gate, after which every read returns
 *       {@code -1} <em>without touching</em> the servlet stream;</li>
 *   <li>{@link #detach()} reports whether a read was still in flight, so the
 *       caller can mark the connection non-reusable
 *       ({@code Connection: close}).</li>
 * </ul>
 */
final class GatedRequestInputStream extends InputStream {

    private final InputStream delegate;
    private final AtomicInteger inFlight = new AtomicInteger();
    private volatile boolean closed;

    GatedRequestInputStream(InputStream delegate) {
        this.delegate = Objects.requireNonNull(delegate, "delegate");
    }

    @Override
    public int read() throws IOException {
        if (closed) {
            return -1;
        }
        inFlight.incrementAndGet();
        try {
            return closed ? -1 : delegate.read();
        } finally {
            inFlight.decrementAndGet();
        }
    }

    @Override
    public int read(byte[] b, int off, int len) throws IOException {
        Objects.checkFromIndexSize(off, len, b.length);
        if (closed) {
            return -1;
        }
        if (len == 0) {
            return 0;
        }
        inFlight.incrementAndGet();
        try {
            return closed ? -1 : delegate.read(b, off, len);
        } finally {
            inFlight.decrementAndGet();
        }
    }

    @Override
    public int available() throws IOException {
        return closed ? 0 : delegate.available();
    }

    /**
     * Close the gate and forward the close to the servlet stream (the native
     * dispatch calls this to unblock a producer parked in {@code read}).
     */
    @Override
    public void close() throws IOException {
        closed = true;
        delegate.close();
    }

    /**
     * Close the gate without touching the delegate.  Returns {@code true}
     * when a read on the delegate was still in flight — the servlet stream
     * position is then unknown and the connection must not be reused.
     */
    boolean detach() {
        closed = true;
        return inFlight.get() > 0;
    }

    /** Whether the gate is closed (reads return {@code -1}). */
    boolean isClosed() {
        return closed;
    }
}
