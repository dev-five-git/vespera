package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.ReadListener;
import jakarta.servlet.ServletInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.lang.reflect.Field;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.CompletionException;
import java.util.concurrent.Executor;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.core.io.Resource;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.web.server.ResponseStatusException;

class VesperaProxyControllerCoverageTest {

    private String previousOversizePolicy;

    @AfterEach
    void cleanThreadStateAndProperties() {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        VesperaBridge.clearCurrentThreadBuffers();
        if (previousOversizePolicy == null) {
            System.clearProperty("vespera.direct.oversize-policy");
        } else {
            System.setProperty("vespera.direct.oversize-policy", previousOversizePolicy);
        }
    }

    @Test
    void compatibilityConstructorsDelegateToTheFullConstructor() {
        AppNameResolver app = request -> null;
        DispatchModeResolver mode = request -> DispatchMode.SYNC;
        Executor executor = Runnable::run;

        new VesperaProxyController(app, mode);
        new VesperaProxyController(app, mode, executor, false);
        new VesperaProxyController(app, mode, executor, true, 123);

        NullPointerException missingApp = assertThrows(
                NullPointerException.class,
                () -> new VesperaProxyController(null, mode, executor, true, 1, 1));
        assertEquals("appResolver", missingApp.getMessage());
    }

    @Test
    void adaptiveModeOnlyDowngradesRememberedDirectRoutes() {
        assertEquals(
                DispatchMode.STREAMING,
                VesperaProxyController.effectiveMode(DispatchMode.DIRECT, true));
        assertEquals(
                DispatchMode.DIRECT,
                VesperaProxyController.effectiveMode(DispatchMode.DIRECT, false));
        assertEquals(
                DispatchMode.SYNC,
                VesperaProxyController.effectiveMode(DispatchMode.SYNC, true));
        assertEquals(0, VesperaDirectBufferPool.initialSmallWriteStreak());
    }

    @Test
    void hardBufferedBodyLimitHasAnAllocationFreeDecisionSeam() {
        long maxBufferedBody = Integer.MAX_VALUE - 8L;

        ResponseStatusException exception = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.rejectAtBufferedBodyLimit(
                        maxBufferedBody, maxBufferedBody));

        assertEquals(413, exception.getStatusCode().value());
        assertTrue(exception.getReason().contains(Long.toString(maxBufferedBody)));
        VesperaProxyController.rejectAtBufferedBodyLimit(maxBufferedBody - 1, maxBufferedBody);
        VesperaProxyController.rejectAtBufferedBodyLimit(maxBufferedBody, maxBufferedBody - 1);
    }

    @Test
    void unknownBodyDefaultLimitHasAnAllocationFreeDecisionSeam() {
        long defaultLimit = 64L * 1024L * 1024L;

        ResponseStatusException exception = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.rejectAboveDefaultBufferedLimit(defaultLimit + 1));

        assertEquals(413, exception.getStatusCode().value());
        assertEquals("buffered request body of unknown length exceeds the built-in "
                + defaultLimit + "-byte bound (vespera.bridge.max-buffered-request-bytes=0; actual "
                + (defaultLimit + 1) + " bytes)", exception.getReason());
        VesperaProxyController.rejectAboveDefaultBufferedLimit(defaultLimit);
    }

    @Test
    void impossibleKnownContentLengthIsRejectedBeforeReading() {
        MockHttpServletRequest request = requestReportingLength(Integer.MAX_VALUE - 7L, new byte[0]);

        ResponseStatusException exception = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.readBody(request, 0));

        assertEquals(413, exception.getStatusCode().value());
        assertTrue(exception.getReason().contains("2147483640"));
    }

    @Test
    void unknownUnlimitedAndLargeKnownBodiesUseTheirBoundedReadPaths() throws Exception {
        MockHttpServletRequest unknown = requestReportingLength(
                -1, "unknown".getBytes(StandardCharsets.UTF_8));
        assertArrayEquals(
                "unknown".getBytes(StandardCharsets.UTF_8),
                VesperaProxyController.readBody(unknown, 0));

        MockHttpServletRequest largeKnown = requestReportingLength(
                64L * 1024L * 1024L + 1L,
                "short-eof".getBytes(StandardCharsets.UTF_8));
        assertArrayEquals(
                "short-eof".getBytes(StandardCharsets.UTF_8),
                VesperaProxyController.readBody(largeKnown, 0));
    }

    @Test
    void tryWithResourcePreservesCloseAndSuppressedReadFailures() {
        IOException closeOnly = assertThrows(
                IOException.class,
                () -> VesperaProxyController.readBody(
                        requestWithStream(new FailingServletInputStream(false)), 0));
        assertEquals("close failure", closeOnly.getMessage());

        IOException readAndClose = assertThrows(
                IOException.class,
                () -> VesperaProxyController.readBody(
                        requestWithStream(new FailingServletInputStream(true)), 0));
        assertEquals("read failure", readAndClose.getMessage());
        assertEquals(1, readAndClose.getSuppressed().length);
        assertEquals("close failure", readAndClose.getSuppressed()[0].getMessage());

        NullPointerException nullStream = assertThrows(
                NullPointerException.class,
                () -> VesperaProxyController.readBody(requestWithStream(null), 0));
        assertTrue(nullStream.getMessage().contains("readNBytes"), nullStream.getMessage());
    }

    @Test
    void legacySyncOverloadReachesTheNativeBoundary() {
        MockHttpServletResponse response = new MockHttpServletResponse();

        assertThrows(
                UnsatisfiedLinkError.class,
                () -> VesperaProxyController.dispatchSync(
                        response,
                        null,
                        "GET",
                        "/health",
                        "",
                        sink -> {},
                        VesperaWireCodec.EMPTY_BODY));
    }

    @Test
    void completeWireWriterOwnsFramingForGetHeadAndNoBodyStatus() throws Exception {
        byte[] ok = heapWire(200, "hello");
        MockHttpServletResponse get = new MockHttpServletResponse();
        WireResponseWriter.writeWireResponse(ok, get);
        assertEquals(200, get.getStatus());
        assertEquals(5, get.getContentLength());
        assertEquals("hello", get.getContentAsString(StandardCharsets.UTF_8));

        MockHttpServletResponse head = new MockHttpServletResponse();
        WireResponseWriter.writeWireResponse(ok, head, "HEAD");
        assertEquals(5, head.getContentLength());
        assertEquals(0, head.getContentAsByteArray().length);

        MockHttpServletResponse noContent = new MockHttpServletResponse();
        WireResponseWriter.writeWireResponse(heapWire(204, "ignored"), noContent, "GET");
        // Unset, not an explicit `Content-Length: 0` (the mock reports 0 for both).
        assertNull(noContent.getHeader("Content-Length"));
        assertEquals(0, noContent.getContentAsByteArray().length);

        assertTrue(WireResponseWriter.responseStatusPermitsBody(99));

        MockHttpServletResponse informational = new MockHttpServletResponse();
        WireResponseWriter.writeWireResponse(
                heapWire(199, "ignored"), informational, "GET");
        assertNull(informational.getHeader("Content-Length"));
        assertEquals(0, informational.getContentAsByteArray().length);
    }

    @Test
    void contextPathGuardCoversNullAndNonPrefixContexts() {
        MockHttpServletRequest nullContext = new MockHttpServletRequest("GET", "/health") {
            @Override
            public String getContextPath() {
                return null;
            }
        };
        nullContext.setRequestURI("/health");
        assertEquals("/health", VesperaProxyController.pathWithinApplication(nullContext));

        MockHttpServletRequest nonPrefix = new MockHttpServletRequest("GET", "/health");
        nonPrefix.setContextPath("/api");
        nonPrefix.setRequestURI("/health");
        assertEquals("/health", VesperaProxyController.pathWithinApplication(nonPrefix));
    }

    @Test
    void streamingHeaderSuppressesHeadBodyAfterBodyPermittingStatus() {
        byte[] header = heapWire(200, "represented");
        MockHttpServletResponse response = new MockHttpServletResponse();

        assertFalse(WireResponseWriter.applyDecodedHeader(header, response, "HEAD"));
        assertEquals(200, response.getStatus());
    }

    @Test
    void repeatedUnsafeDirectDowngradeTakesDebugLoggingBranch() throws Exception {
        Field warnedField = VesperaProxyController.class
                .getDeclaredField("UNSAFE_DIRECT_DOWNGRADE_WARNED");
        warnedField.setAccessible(true);
        AtomicBoolean warned = (AtomicBoolean) warnedField.get(null);
        boolean previousWarned = warned.getAndSet(false);
        ch.qos.logback.classic.Logger logger =
                (ch.qos.logback.classic.Logger) org.slf4j.LoggerFactory
                        .getLogger(VesperaProxyController.class);
        ch.qos.logback.classic.Level previousLevel = logger.getLevel();
        logger.setLevel(ch.qos.logback.classic.Level.DEBUG);
        ch.qos.logback.core.read.ListAppender<ch.qos.logback.classic.spi.ILoggingEvent> logged =
                new ch.qos.logback.core.read.ListAppender<>();
        logged.start();
        logger.addAppender(logged);
        try {
            VesperaProxyController controller = new VesperaProxyController(
                    request -> null, request -> DispatchMode.DIRECT, Runnable::run, false);
            assertThrows(UnsatisfiedLinkError.class, () -> controller.dispatchDirectMode(
                    new MockHttpServletResponse(), null, "POST", "/echo", "",
                    sink -> {}, new byte[] {1}, Boolean.FALSE, null));
            assertThrows(UnsatisfiedLinkError.class, () -> controller.dispatchDirectMode(
                    new MockHttpServletResponse(), null, "POST", "/echo", "",
                    sink -> {}, new byte[] {1}, Boolean.FALSE, null));
            assertTrue(warned.get());
            java.util.List<ch.qos.logback.classic.Level> levels = logged.list.stream()
                    .filter(event -> event.getFormattedMessage().contains("routed unsafe method POST"))
                    .map(ch.qos.logback.classic.spi.ILoggingEvent::getLevel)
                    .toList();
            assertEquals(java.util.List.of(ch.qos.logback.classic.Level.WARN, ch.qos.logback.classic.Level.DEBUG),
                    levels, "one WARN, then DEBUG only");

            logger.setLevel(ch.qos.logback.classic.Level.INFO);
            logged.list.clear();
            assertThrows(UnsatisfiedLinkError.class, () -> controller.dispatchDirectMode(
                    new MockHttpServletResponse(), null, "POST", "/echo", "",
                    sink -> {}, new byte[] {1}, Boolean.FALSE, null));
            assertTrue(logged.list.stream().noneMatch(
                    event -> event.getFormattedMessage().contains("routed unsafe method")),
                    "nothing at INFO once warned");

            // With an exchange the downgrade is what gets reported.
            ProxyExchange exchange = new ProxyExchange(
                    org.slf4j.LoggerFactory.getLogger(VesperaProxyControllerCoverageTest.class),
                    java.util.List.of(), "POST", null);
            exchange.mode = DispatchMode.DIRECT;
            assertThrows(UnsatisfiedLinkError.class, () -> controller.dispatchDirectMode(
                    new MockHttpServletResponse(), null, "POST", "/echo", "",
                    sink -> {}, new byte[] {1}, Boolean.FALSE, exchange));
            assertEquals(DispatchMode.SYNC, exchange.mode);
        } finally {
            logger.detachAppender(logged);
            logger.setLevel(previousLevel);
            warned.set(previousWarned);
        }
    }

    @Test
    void aDirectRequestServedThroughAnotherPathReportsThatMode() throws Exception {
        Field loaded = VesperaBridge.class.getDeclaredField("loaded");
        loaded.setAccessible(true);
        boolean wasLoaded = loaded.getBoolean(null);
        previousOversizePolicy = System.getProperty("vespera.direct.oversize-policy");
        java.util.List<DispatchEvent> events = new java.util.concurrent.CopyOnWriteArrayList<>();
        VesperaProxyController controller = new VesperaProxyController(
                request -> null, request -> DispatchMode.DIRECT, Runnable::run, true, 0, 0, null,
                java.util.List.of(events::add));
        try {
            // Past the isInitialized() gate; every native call then fails
            // with UnsatisfiedLinkError (answered 503) after the mode is final.
            loaded.setBoolean(null, true);

            // An unsafe method is downgraded to SYNC.
            MockHttpServletRequest post = new MockHttpServletRequest("POST", "/echo");
            post.setContent(new byte[] {1});
            MockHttpServletResponse posted = new MockHttpServletResponse();
            controller.proxy(post, posted);
            assertEquals(503, posted.getStatus());
            assertEquals(DispatchMode.SYNC, events.get(0).mode());
            assertEquals("/echo", events.get(0).path());

            // oversize-policy=throw: a request beyond the pool cap is re-routed
            // through response streaming before anything is dispatched.
            System.setProperty("vespera.direct.oversize-policy", "throw");
            MockHttpServletRequest big = new MockHttpServletRequest("GET", "/big");
            big.setContent(new byte[4 * 1024 * 1024 + 1]);
            MockHttpServletResponse bigResponse = new MockHttpServletResponse();
            controller.proxy(big, bigResponse);
            assertEquals(503, bigResponse.getStatus());
            assertEquals(DispatchMode.STREAMING, events.get(1).mode());

            // A plain safe DIRECT request keeps reporting DIRECT.
            MockHttpServletResponse got = new MockHttpServletResponse();
            controller.proxy(new MockHttpServletRequest("GET", "/small"), got);
            assertEquals(503, got.getStatus());
            assertEquals(DispatchMode.DIRECT, events.get(2).mode());
        } finally {
            loaded.setBoolean(null, wasLoaded);
        }
    }

    @Test
    void cyclicAsyncFailureTerminatesCauseScanAndPrewrappedFailureIsPreserved() {
        RuntimeException cyclic = new RuntimeException("cycle") {
            @Override
            public synchronized Throwable getCause() {
                return this;
            }
        };
        assertFalse(AsyncProxyResponses.isRejectedExecution(cyclic));

        // A two-element cycle through the standard getCause() (which maps a
        // self-cause to null, so the check above alone never meets it).
        RuntimeException a = new RuntimeException("a");
        RuntimeException b = new RuntimeException("b", a);
        a.initCause(b);
        org.junit.jupiter.api.Assertions.assertTimeoutPreemptively(java.time.Duration.ofSeconds(5), () -> {
            assertFalse(AsyncProxyResponses.isRejectedExecution(a));
            assertEquals(500, AsyncProxyResponses.asyncErrorStatus(a));
            assertEquals(500, AsyncProxyResponses.asyncFailureToResponse(a).getStatusCode().value());
        });

        ResponseEntity<?> bare = AsyncProxyResponses.asyncFailureToResponse(
                new IllegalStateException());
        assertEquals(500, bare.getStatusCode().value());
        assertEquals("vespera dispatch failed",
                new String((byte[]) bare.getBody(), StandardCharsets.UTF_8));
        ResponseEntity<?> emptyCompletion = AsyncProxyResponses.asyncFailureToResponse(
                new CompletionException(null));
        assertEquals(500, emptyCompletion.getStatusCode().value());
    }

    @Test
    void zeroContentLengthOnABodiedRequestIsReadAsUnknownLength() throws java.io.IOException {
        // CL 0 + Transfer-Encoding (not provably bodyless): the cap still applies.
        org.springframework.web.server.ResponseStatusException tooLarge = org.junit.jupiter.api.Assertions.assertThrows(
                org.springframework.web.server.ResponseStatusException.class,
                () -> VesperaProxyController.readBufferedBody(
                        new java.io.ByteArrayInputStream(new byte[20]), 0, 10));
        assertEquals(413, tooLarge.getStatusCode().value());
        assertEquals(5, VesperaProxyController.readBufferedBody(
                new java.io.ByteArrayInputStream(new byte[5]), 0, 10).length);
        assertEquals(20, VesperaProxyController.readBufferedBody(
                new java.io.ByteArrayInputStream(new byte[20]), 0, 0).length);
    }

    @Test
    void asyncClientDisconnectIsReported499AndOtherErrors500() {
        java.util.List<DispatchEvent> seen = new java.util.ArrayList<>();
        org.slf4j.Logger quiet = org.slf4j.LoggerFactory.getLogger("test.async-error");

        ProxyExchange gone = new ProxyExchange(quiet, java.util.List.of(seen::add), "GET", "rid");
        gone.reachedRust = true;
        AsyncProxyResponses.reportAsyncError(gone,
                new IllegalStateException(new java.io.IOException("Broken pipe")), false);
        assertEquals(499, seen.get(0).status());
        assertFalse(gone.reachedRust, "a disconnect is not an answer of the selected app");

        ProxyExchange failed = new ProxyExchange(quiet, java.util.List.of(seen::add), "GET", "rid");
        failed.reachedRust = true;
        AsyncProxyResponses.reportAsyncError(failed, new IllegalStateException("boom"), false);
        assertEquals(500, seen.get(1).status());
        assertTrue(failed.reachedRust);
    }

    @Test
    void retryDecisionRequiresBothConfigurationAndSafeMethod() {
        assertTrue(DirectModeDispatcher.shouldRetry(true, "GET"));
        assertFalse(DirectModeDispatcher.shouldRetry(false, "GET"));
        assertFalse(DirectModeDispatcher.shouldRetry(true, "POST"));
    }

    @Test
    void directBodyScratchGrowsResetsAndShrinksWhilePreservingBytes() throws Exception {
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
        byte[] large = new byte[300 * 1024];
        java.util.Arrays.fill(large, (byte) 7);
        ByteArrayOutputStream largeOut = new ByteArrayOutputStream();
        VesperaDirectBufferPool.writeDirectBody(ByteBuffer.wrap(large), largeOut);
        assertArrayEquals(large, largeOut.toByteArray());
        assertEquals(large.length, VesperaDirectBufferPool.directBodyScratchCapacityForTest(), "grown");

        for (int i = 0; i < 8; i++) {
            // Retained (above the 256 KiB retain cap) for 7 small writes...
            assertEquals(large.length, VesperaDirectBufferPool.directBodyScratchCapacityForTest(), "write " + i);
            ByteArrayOutputStream smallOut = new ByteArrayOutputStream();
            VesperaDirectBufferPool.writeDirectBody(
                    ByteBuffer.wrap(new byte[] {(byte) i}), smallOut);
            assertArrayEquals(new byte[] {(byte) i}, smallOut.toByteArray());
        }
        // ...and shrunk back to the 16 KiB initial size by the 8th.
        assertEquals(16 * 1024, VesperaDirectBufferPool.directBodyScratchCapacityForTest());
    }

    @Test
    void bodyPermittingStreamCoversSingleByteSuppressionAndFlush() throws Exception {
        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        WireResponseWriter.BodyPermittingOutputStream out =
                new WireResponseWriter.BodyPermittingOutputStream(sink, "GET");

        out.write('a');
        out.applyPermitsBody(false);
        out.write('b');
        out.write("blocked".getBytes(StandardCharsets.UTF_8), 0, 7);
        out.flush();

        assertEquals("a", sink.toString(StandardCharsets.UTF_8));
    }

    @Test
    void asyncBuilderExposesTheBodyAndDropsItForNoContent() throws Exception {
        ResponseEntity<?> response =
                WireResponseWriter.buildAsyncResponseEntityFromWire(heapWire(200, "resource"), null, null);

        assertEquals(200, response.getStatusCode().value());
        assertEquals(8, response.getHeaders().getContentLength());
        try (java.io.InputStream in = ((org.springframework.core.io.Resource) response.getBody())
                .getInputStream()) {
            assertArrayEquals("resource".getBytes(StandardCharsets.UTF_8), in.readAllBytes());
        }

        ResponseEntity<?> noContent =
                WireResponseWriter.buildAsyncResponseEntityFromWire(heapWire(204, "ignored"), null, null);
        assertEquals(-1, noContent.getHeaders().getContentLength());
        assertNull(noContent.getBody());
    }

    private static MockHttpServletRequest requestReportingLength(long length, byte[] body) {
        MockHttpServletRequest request = new MockHttpServletRequest("POST", "/echo") {
            @Override
            public long getContentLengthLong() {
                return length;
            }
        };
        request.setContent(body);
        return request;
    }

    private static MockHttpServletRequest requestWithStream(ServletInputStream stream) {
        return new MockHttpServletRequest("POST", "/echo") {
            @Override
            public long getContentLengthLong() {
                return 1;
            }

            @Override
            public ServletInputStream getInputStream() {
                return stream;
            }
        };
    }

    private static final class FailingServletInputStream extends ServletInputStream {
        private final boolean failRead;
        private boolean read;

        private FailingServletInputStream(boolean failRead) {
            this.failRead = failRead;
        }

        @Override
        public int read() throws IOException {
            if (failRead) {
                throw new IOException("read failure");
            }
            if (read) {
                return -1;
            }
            read = true;
            return 7;
        }

        @Override
        public void close() throws IOException {
            throw new IOException("close failure");
        }

        @Override
        public boolean isFinished() {
            return read;
        }

        @Override
        public boolean isReady() {
            return true;
        }

        @Override
        public void setReadListener(ReadListener listener) {}
    }

    private static byte[] heapWire(int status, String body) {
        String json = "{\"v\":1,\"status\":" + status
                + ",\"headers\":{\"content-type\":\"text/plain\"},\"metadata\":{}}";
        byte[] header = json.getBytes(StandardCharsets.UTF_8);
        byte[] bodyBytes = body.getBytes(StandardCharsets.UTF_8);
        ByteBuffer wire = ByteBuffer.allocate(4 + header.length + bodyBytes.length);
        wire.putInt(header.length);
        wire.put(header);
        wire.put(bodyBytes);
        return wire.array();
    }
}
