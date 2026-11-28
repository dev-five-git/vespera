package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.ServletOutputStream;
import jakarta.servlet.WriteListener;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.List;
import org.junit.jupiter.api.Test;
import org.slf4j.LoggerFactory;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * A header-first stream that fails after the Rust header was applied: a
 * still-uncommitted response is replaced by a proxy {@code 500} (flagged
 * panicked for a Rust panic), a committed one rethrows so the container
 * aborts the connection — never a cleanly completed short {@code 200}.
 */
class StreamAbortHandlingTest {

    private static final String BODY_ABORTED =
            "vespera: response body stream aborted after the header was committed";

    /** The Rust header was applied: 200, an upstream header, part of the body buffered. */
    private static ProxyServletResponse appliedRustHeader(MockHttpServletResponse servlet)
            throws IOException {
        ProxyServletResponse response = new ProxyServletResponse(servlet, "X-Request-Id", "rid-1");
        response.applyDefaultRequestId();
        response.setStatus(200);
        response.addHeader("Content-Type", "application/octet-stream");
        response.addHeader("X-Upstream", "rust");
        response.getOutputStream().write(new byte[] {1, 2, 3});
        return response;
    }

    @Test
    void panicMessageIsRecognised() {
        assertTrue(new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE).panicked());
        assertFalse(new VesperaStreamAbortedException(BODY_ABORTED).panicked());
        assertTrue(VesperaStreamAbortedException.isPanic(
                new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE)));
        assertFalse(VesperaStreamAbortedException.isPanic(new VesperaStreamAbortedException(BODY_ABORTED)));
        // Fallback when the typed class could not be resolved natively.
        assertTrue(VesperaStreamAbortedException.isPanic(
                new IOException(VesperaStreamAbortedException.PANIC_MESSAGE)));
        assertFalse(VesperaStreamAbortedException.isPanic(new IOException(BODY_ABORTED)));
        assertFalse(VesperaStreamAbortedException.isPanic(new IOException()));
    }

    @Test
    void panicAfterAFailedHeaderCallbackIsStillRecognised() {
        // The header-failed text is kept, but the panic bit must reach Java.
        String message = VesperaStreamAbortedException.HEADER_CALLBACK_PANIC_MESSAGE;
        assertTrue(message.startsWith("vespera: response header callback failed before body streaming"));
        assertTrue(new VesperaStreamAbortedException(message).panicked());
        assertTrue(VesperaStreamAbortedException.isPanic(new IOException(message)));
        assertFalse(new VesperaStreamAbortedException(
                "vespera: response header callback failed before body streaming").panicked());
    }

    @Test
    void uncommittedPanicIsReplacedByAPanicked500() throws IOException {
        MockHttpServletResponse servlet = new MockHttpServletResponse();
        ProxyServletResponse response = appliedRustHeader(servlet);

        DirectModeDispatcher.streamHeaderFirst(response, () -> {
            throw new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE);
        });

        assertEquals(500, servlet.getStatus());
        assertEquals(ProxyErrors.CONTENT_TYPE, servlet.getContentType());
        assertEquals(ProxyErrors.RUST_PANIC, servlet.getContentAsString(StandardCharsets.UTF_8));
        assertNull(servlet.getHeader("X-Upstream"), "Rust headers are dropped");
        assertEquals("rid-1", servlet.getHeader("X-Request-Id"));
        assertNull(servlet.getHeader("Connection"));
        assertTrue(response.panicked());
    }

    @Test
    void uncommittedBodyAbortIsReplacedByA500() throws IOException {
        MockHttpServletResponse servlet = new MockHttpServletResponse();
        ProxyServletResponse response = appliedRustHeader(servlet);

        assertTrue(ProxyErrors.writeStreamAbort(response, new IOException(BODY_ABORTED), true));

        assertEquals(500, servlet.getStatus());
        assertEquals(ProxyErrors.STREAM_ABORTED, servlet.getContentAsString(StandardCharsets.UTF_8));
        assertEquals("close", servlet.getHeader("Connection"));
        assertFalse(response.panicked());
    }

    @Test
    void committedAbortIsRethrownButStillFlagsThePanic() throws IOException {
        MockHttpServletResponse servlet = new MockHttpServletResponse();
        ProxyServletResponse response = appliedRustHeader(servlet);
        response.flushBuffer();
        VesperaStreamAbortedException panic =
                new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE);

        IOException thrown = assertThrows(IOException.class,
                () -> DirectModeDispatcher.streamHeaderFirst(response, () -> {
                    throw panic;
                }));

        assertSame(panic, thrown);
        assertEquals(200, servlet.getStatus());
        assertTrue(response.panicked());
    }

    @Test
    void plainServletResponseIsResetToo() throws IOException {
        MockHttpServletResponse servlet = new MockHttpServletResponse();
        servlet.setStatus(200);
        servlet.getOutputStream().write(new byte[] {9});

        assertTrue(ProxyErrors.writeStreamAbort(servlet, new IOException(BODY_ABORTED), false));

        assertEquals(500, servlet.getStatus());
        assertEquals(ProxyErrors.STREAM_ABORTED, servlet.getContentAsString(StandardCharsets.UTF_8));
    }

    @Test
    void completedStreamIsFlushed() throws IOException {
        MockHttpServletResponse servlet = new MockHttpServletResponse();
        boolean[] ran = {false};

        DirectModeDispatcher.streamHeaderFirst(servlet, () -> ran[0] = true);

        assertTrue(ran[0]);
        assertTrue(servlet.isCommitted());
    }

    /** A servlet response whose client went away: every write after {@link #disconnect()} fails. */
    private static final class DisconnectedResponse extends MockHttpServletResponse {
        private boolean gone;

        void disconnect() {
            gone = true;
        }

        @Override
        public ServletOutputStream getOutputStream() {
            ServletOutputStream real = super.getOutputStream();
            if (!gone) {
                return real;
            }
            return new ServletOutputStream() {
                @Override
                public boolean isReady() {
                    return true;
                }

                @Override
                public void setWriteListener(WriteListener listener) {
                    // not used by the proxy
                }

                @Override
                public void write(int b) throws IOException {
                    throw new IOException("broken pipe");
                }
            };
        }
    }

    @Test
    void aClientGoneWhileTheAbortIsAnsweredKeepsTheStreamFailureAsTheCause() throws IOException {
        DisconnectedResponse servlet = new DisconnectedResponse();
        ProxyServletResponse response = appliedRustHeader(servlet);
        servlet.disconnect();
        VesperaStreamAbortedException panic =
                new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE);

        IOException thrown = assertThrows(IOException.class,
                () -> DirectModeDispatcher.streamHeaderFirst(response, () -> {
                    throw panic;
                }));

        assertSame(panic, thrown, "the stream failure is rethrown, not the broken pipe");
        assertEquals(1, thrown.getSuppressed().length);
        assertEquals("broken pipe", thrown.getSuppressed()[0].getMessage());
        assertTrue(response.panicked());
    }

    @Test
    void aClientGoneWhileTheBidirectionalAbortIsAnsweredKeepsTheStreamFailureAsTheCause()
            throws IOException {
        DisconnectedResponse servlet = new DisconnectedResponse();
        ProxyServletResponse response = appliedRustHeader(servlet);
        servlet.disconnect();
        ProxyExchange exchange = new ProxyExchange(
                LoggerFactory.getLogger(VesperaProxyController.class), List.of(), "POST", "rid-1");
        IOException failure = new IOException("body broke");

        IOException thrown = assertThrows(IOException.class,
                () -> VesperaProxyController.streamBidirectional(() -> {
                    throw failure;
                }, response, new GatedRequestInputStream(new ByteArrayInputStream(new byte[0])),
                        exchange));

        assertSame(failure, thrown, "the stream failure is rethrown, not the broken pipe");
        assertEquals("broken pipe", thrown.getSuppressed()[0].getMessage());
    }

    /** A servlet response whose writer was taken by a filter: {@code getOutputStream} refuses. */
    private static final class WriterTakenResponse extends MockHttpServletResponse {
        private boolean writerTaken;

        void takeWriter() {
            writerTaken = true;
        }

        @Override
        public ServletOutputStream getOutputStream() {
            if (writerTaken) {
                throw new IllegalStateException("getWriter() has already been called for this response");
            }
            return super.getOutputStream();
        }
    }

    @Test
    void aRuntimeFailureWritingTheProxy500KeepsTheStreamFailureAsTheCause() throws IOException {
        WriterTakenResponse servlet = new WriterTakenResponse();
        ProxyServletResponse response = appliedRustHeader(servlet);
        servlet.takeWriter();
        VesperaStreamAbortedException panic =
                new VesperaStreamAbortedException(VesperaStreamAbortedException.PANIC_MESSAGE);

        IOException thrown = assertThrows(IOException.class,
                () -> DirectModeDispatcher.streamHeaderFirst(response, () -> {
                    throw panic;
                }));

        assertSame(panic, thrown, "the stream failure is rethrown, not the writer IllegalStateException");
        assertEquals(1, thrown.getSuppressed().length);
        assertEquals(IllegalStateException.class, thrown.getSuppressed()[0].getClass());
        assertTrue(response.panicked());
    }
}
