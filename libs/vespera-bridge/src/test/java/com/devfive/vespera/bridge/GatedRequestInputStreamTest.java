package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * The bidirectional request-body gate: a producer detached by the Rust
 * dispatch must never read the servlet stream after the dispatch returned,
 * and a read still parked at that moment marks the connection non-reusable.
 */
class GatedRequestInputStreamTest {

    /** Counts delegate reads so a post-detach read can be proven absent. */
    private static final class CountingStream extends InputStream {
        final AtomicInteger reads = new AtomicInteger();
        final AtomicBoolean closed = new AtomicBoolean();
        private final ByteArrayInputStream in;

        CountingStream(byte[] data) {
            in = new ByteArrayInputStream(data);
        }

        @Override
        public int read() {
            reads.incrementAndGet();
            return in.read();
        }

        @Override
        public int read(byte[] b, int off, int len) {
            reads.incrementAndGet();
            return in.read(b, off, len);
        }

        @Override
        public int available() {
            return in.available();
        }

        @Override
        public void close() {
            closed.set(true);
        }
    }

    @Test
    void passesReadsThroughUntilDetachedThenReturnsEofWithoutTouchingTheDelegate()
            throws IOException {
        CountingStream servlet = new CountingStream(new byte[] {1, 2, 3, 4});
        GatedRequestInputStream gate = new GatedRequestInputStream(servlet);
        assertEquals(1, gate.read());
        byte[] buf = new byte[2];
        assertEquals(2, gate.read(buf, 0, 2));
        assertArrayEquals(new byte[] {2, 3}, buf);
        assertEquals(0, gate.read(buf, 0, 0));
        assertEquals(1, gate.available());
        assertEquals(2, servlet.reads.get());

        assertFalse(gate.detach(), "no read was in flight");
        assertTrue(gate.isClosed());
        assertEquals(-1, gate.read());
        assertEquals(-1, gate.read(buf, 0, 2));
        assertEquals(0, gate.available());
        assertEquals(2, servlet.reads.get(), "the servlet stream is never read after detach");
        assertFalse(servlet.closed.get(), "detach does not close the servlet stream");
        assertThrows(IndexOutOfBoundsException.class, () -> gate.read(buf, 1, 5));
    }

    @Test
    void closeForwardsToTheDelegateAndGatesLaterReads() throws IOException {
        CountingStream servlet = new CountingStream(new byte[] {9});
        GatedRequestInputStream gate = new GatedRequestInputStream(servlet);
        gate.close();
        assertTrue(servlet.closed.get(), "the native close still unblocks the servlet stream");
        assertEquals(-1, gate.read());
        assertEquals(0, servlet.reads.get());
    }

    @Test
    void detachDuringAParkedReadReportsInFlightAndClosesTheConnection() throws Exception {
        CountDownLatch entered = new CountDownLatch(1);
        CountDownLatch release = new CountDownLatch(1);
        InputStream parked = new InputStream() {
            @Override
            public int read() throws IOException {
                entered.countDown();
                try {
                    release.await();
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    throw new IOException(e);
                }
                return 'x';
            }
        };
        GatedRequestInputStream gate = new GatedRequestInputStream(parked);
        AtomicReference<Integer> got = new AtomicReference<>();
        Thread producer = new Thread(() -> {
            try {
                got.set(gate.read());
            } catch (IOException e) {
                got.set(Integer.MIN_VALUE);
            }
        });
        producer.start();
        assertTrue(entered.await(10, TimeUnit.SECONDS));

        MockHttpServletResponse uncommitted = new MockHttpServletResponse();
        assertTrue(VesperaProxyController.detachRequestBody(gate, uncommitted));
        assertEquals("close", uncommitted.getHeader("Connection"));

        MockHttpServletResponse committed = new MockHttpServletResponse();
        committed.setCommitted(true);
        assertTrue(VesperaProxyController.detachRequestBody(gate, committed),
                "best effort once committed");
        assertNull(committed.getHeader("Connection"));

        release.countDown();
        producer.join(10_000);
        assertEquals('x', got.get(), "the in-flight read finishes on its own");
        assertEquals(-1, gate.read(), "but no later read reaches the servlet stream");

        MockHttpServletResponse idle = new MockHttpServletResponse();
        assertFalse(VesperaProxyController.detachRequestBody(
                new GatedRequestInputStream(InputStream.nullInputStream()), idle));
        assertNull(idle.getHeader("Connection"), "a fully drained request keeps keep-alive");
    }
}
