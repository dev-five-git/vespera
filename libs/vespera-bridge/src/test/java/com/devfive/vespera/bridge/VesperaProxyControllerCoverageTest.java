package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
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
        VesperaProxyController.clearCurrentThreadBuffers();
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
        assertEquals(0, VesperaProxyController.initialSmallWriteStreak());
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
        assertTrue(exception.getReason().contains(Long.toString(defaultLimit)));
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
        VesperaProxyController.writeWireResponse(ok, get);
        assertEquals(200, get.getStatus());
        assertEquals(5, get.getContentLength());
        assertEquals("hello", get.getContentAsString(StandardCharsets.UTF_8));

        MockHttpServletResponse head = new MockHttpServletResponse();
        VesperaProxyController.writeWireResponse(ok, head, "HEAD");
        assertEquals(5, head.getContentLength());
        assertEquals(0, head.getContentAsByteArray().length);

        MockHttpServletResponse noContent = new MockHttpServletResponse();
        VesperaProxyController.writeWireResponse(heapWire(204, "ignored"), noContent, "GET");
        assertEquals(0, noContent.getContentLength());
        assertEquals(0, noContent.getContentAsByteArray().length);
    }

    @Test
    void cyclicAsyncFailureTerminatesCauseScanAndPrewrappedFailureIsPreserved() {
        RuntimeException cyclic = new RuntimeException("cycle") {
            @Override
            public synchronized Throwable getCause() {
                return this;
            }
        };
        assertFalse(VesperaProxyController.isRejectedExecution(cyclic));

        CompletionException wrapped = new CompletionException(new IllegalStateException("boom"));
        CompletionException propagated = assertThrows(
                CompletionException.class,
                () -> VesperaProxyController.asyncFailureToResponse(wrapped));
        assertSame(wrapped, propagated);
    }

    @Test
    void retryDecisionRequiresBothConfigurationAndSafeMethod() {
        assertTrue(VesperaProxyController.shouldRetryDirect(true, "GET"));
        assertFalse(VesperaProxyController.shouldRetryDirect(false, "GET"));
        assertFalse(VesperaProxyController.shouldRetryDirect(true, "POST"));
    }

    @Test
    void virtualThreadOverflowWithoutRetryWritesActionable500() throws Exception {
        previousOversizePolicy = System.getProperty("vespera.direct.oversize-policy");
        System.setProperty("vespera.direct.oversize-policy", "throw");
        VesperaProxyController controller = new VesperaProxyController(
                request -> null,
                request -> DispatchMode.DIRECT,
                Runnable::run,
                false);
        MockHttpServletResponse response = new MockHttpServletResponse();

        controller.dispatchDirectMode(
                response,
                null,
                "GET",
                "/health",
                "",
                sink -> {},
                VesperaWireCodec.EMPTY_BODY,
                Boolean.TRUE);

        assertEquals(500, response.getStatus());
        assertEquals("text/plain; charset=utf-8", response.getContentType());
        String body = response.getContentAsString(StandardCharsets.UTF_8);
        assertTrue(body.startsWith("vespera DIRECT overflow: response needs "), body);
        assertTrue(body.endsWith("bytes; route this request via BIDIRECTIONAL_STREAMING"), body);
        assertEquals(response.getContentAsByteArray().length, response.getContentLength());
    }

    @Test
    void directBodyScratchGrowsResetsAndShrinksWhilePreservingBytes() throws Exception {
        VesperaProxyController.clearCurrentThreadBuffers();
        byte[] large = new byte[300 * 1024];
        java.util.Arrays.fill(large, (byte) 7);
        ByteArrayOutputStream largeOut = new ByteArrayOutputStream();
        VesperaProxyController.writeDirectBody(ByteBuffer.wrap(large), largeOut);
        assertArrayEquals(large, largeOut.toByteArray());

        for (int i = 0; i < 8; i++) {
            ByteArrayOutputStream smallOut = new ByteArrayOutputStream();
            VesperaProxyController.writeDirectBody(
                    ByteBuffer.wrap(new byte[] {(byte) i}), smallOut);
            assertArrayEquals(new byte[] {(byte) i}, smallOut.toByteArray());
        }
    }

    @Test
    void bodyPermittingStreamCoversSingleByteSuppressionAndFlush() throws Exception {
        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        VesperaProxyController.BodyPermittingOutputStream out =
                new VesperaProxyController.BodyPermittingOutputStream(sink, "GET");

        out.write('a');
        out.applyPermitsBody(false);
        out.write('b');
        out.write("blocked".getBytes(StandardCharsets.UTF_8), 0, 7);
        out.flush();

        assertEquals("a", sink.toString(StandardCharsets.UTF_8));
    }

    @Test
    void convenienceAsyncBuilderExposesAReadableResource() throws Exception {
        ResponseEntity<?> response =
                VesperaProxyController.buildResponseEntityFromWire(heapWire(200, "resource"));
        Resource resource = (Resource) response.getBody();

        assertEquals(200, response.getStatusCode().value());
        assertEquals(8, resource.contentLength());
        assertEquals("vespera wire response body slice", resource.getDescription());
        assertArrayEquals(
                "resource".getBytes(StandardCharsets.UTF_8),
                resource.getInputStream().readAllBytes());

        ResponseEntity<?> noContent =
                VesperaProxyController.buildResponseEntityFromWire(heapWire(204, "ignored"));
        assertEquals(0, noContent.getHeaders().getContentLength());
        assertEquals(0, ((Resource) noContent.getBody()).contentLength());
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
