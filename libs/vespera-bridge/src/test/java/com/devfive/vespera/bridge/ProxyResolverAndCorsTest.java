package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.assertThrows;

import jakarta.servlet.http.HttpServletRequest;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;
import org.springframework.http.HttpHeaders;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * Controller-level dispatch decisions and the {@link ProxyServletResponse}
 * CORS bookkeeping, without a native library.
 */
class ProxyResolverAndCorsTest {

    private String previousOversizePolicy;

    @AfterEach
    void restoreOversizePolicy() {
        if (previousOversizePolicy == null) {
            System.clearProperty("vespera.direct.oversize-policy");
        } else {
            System.setProperty("vespera.direct.oversize-policy", previousOversizePolicy);
        }
        VesperaDirectBufferPool.clearCurrentThreadBuffers();
    }

    /** Whether a frame of {@code owner} is on the throwable's stack. */
    private static boolean passesThrough(Throwable failure, Class<?> owner) {
        for (StackTraceElement frame : failure.getStackTrace()) {
            if (frame.getClassName().equals(owner.getName())) {
                return true;
            }
        }
        return false;
    }

    private static byte[] wire(String header, String body) {
        byte[] h = header.getBytes(StandardCharsets.UTF_8);
        byte[] b = body.getBytes(StandardCharsets.UTF_8);
        return ByteBuffer.allocate(4 + h.length + b.length).putInt(h.length).put(h).put(b).array();
    }

    // ── dispatch-mode resolution ───────────────────────────────────────────

    /** Sends {@code /files/**} to STREAMING, everything else to the built-in choice. */
    static final class FilesStreamResolver extends SmartDispatchModeResolver {
        @Override
        public DispatchMode resolveMode(HttpServletRequest request) {
            if (request.getRequestURI().startsWith("/files/")) {
                return DispatchMode.STREAMING;
            }
            return super.resolveMode(request);
        }
    }

    private static DispatchMode modeChosenFor(DispatchModeResolver resolver, String path) throws Exception {
        List<DispatchEvent> events = new CopyOnWriteArrayList<>();
        VesperaProxyController controller = new VesperaProxyController(
                request -> null, resolver, Runnable::run, true, 0, 0, null, List.of(events::add));
        MockHttpServletRequest request = new MockHttpServletRequest("GET", path);
        request.setRequestURI(path);
        MockHttpServletResponse response = new MockHttpServletResponse();
        controller.proxy(request, response);
        // No native library here: answered 503 after the mode was chosen.
        assertEquals(503, response.getStatus());
        assertEquals(1, events.size());
        return events.get(0).mode();
    }

    @Test
    void aSubclassOverrideOfTheSmartResolverIsHonoured() throws Exception {
        assertEquals(DispatchMode.STREAMING, modeChosenFor(new FilesStreamResolver(), "/files/report.pdf"));
        assertEquals(DispatchMode.DIRECT, modeChosenFor(new FilesStreamResolver(), "/health"));
        assertEquals(DispatchMode.DIRECT, modeChosenFor(new SmartDispatchModeResolver(), "/files/report.pdf"));
    }

    // ── DIRECT: oversize-policy=throw is not a response overflow ──────────

    @Test
    void oversizePolicyRejectionStreamsWithoutRecordingAnOverflow() {
        previousOversizePolicy = System.getProperty("vespera.direct.oversize-policy");
        System.setProperty("vespera.direct.oversize-policy", "throw");
        for (boolean retry : new boolean[] {true, false}) {
            VesperaProxyController controller = new VesperaProxyController(
                    request -> null, request -> DispatchMode.DIRECT, Runnable::run, retry);
            MockHttpServletResponse response = new MockHttpServletResponse();
            // A virtual thread under oversize-policy=throw: rejected before any
            // dispatch, so the request is streamed (the native streaming
            // symbol is missing here) instead of answering a "DIRECT overflow"
            // 500 or remembering the route as an overflowing one. Both paths
            // end in UnsatisfiedLinkError, so the stack says which one ran: the
            // heap fallback reaches ParkingDispatch, the streamed one does not.
            UnsatisfiedLinkError failure = assertThrows(UnsatisfiedLinkError.class,
                    () -> controller.dispatchDirectMode(
                            response, null, "GET", "/health", "", sink -> { },
                            VesperaWireCodec.EMPTY_BODY, Boolean.TRUE, null));
            assertFalse(passesThrough(failure, ParkingDispatch.class),
                    "oversize-policy=throw must not fall back to the heap dispatch");
            assertTrue(passesThrough(failure, DirectModeDispatcher.class),
                    "the rejected request is streamed by DirectModeDispatcher");
            assertEquals(0, response.getContentAsByteArray().length, "no proxy-written 500");
            assertFalse(controller.avoidsDirect(null, "GET", "/health", ""), "no overflow recorded");
        }
    }

    // ── ASYNC HEAD ─────────────────────────────────────────────────────────

    @Test
    void asyncHeadWithoutADeclaredLengthHasNoBodyObject() {
        byte[] noLength = wire("{\"status\":200,\"headers\":{\"content-type\":\"text/plain\"}}", "");
        ResponseEntity<?> head = WireResponseWriter.buildAsyncResponseEntityFromWire(noLength, "HEAD", null);
        assertEquals(200, head.getStatusCode().value());
        // A body object (even an empty array) lets a message converter add
        // `Content-Length: 0`; SYNC / DIRECT leave the length unset.
        assertNull(head.getBody());
        assertNull(head.getHeaders().getFirst(HttpHeaders.CONTENT_LENGTH));

        byte[] declared = wire("{\"status\":200,\"headers\":{\"content-length\":\"42\"}}", "");
        ResponseEntity<?> withLength = WireResponseWriter.buildAsyncResponseEntityFromWire(declared, "HEAD", null);
        assertNull(withLength.getBody());
        assertEquals("42", withLength.getHeaders().getFirst(HttpHeaders.CONTENT_LENGTH));

        ResponseEntity<?> get = WireResponseWriter.buildAsyncResponseEntityFromWire(noLength, "GET", null);
        assertSame(VesperaWireCodec.EMPTY_BODY, get.getBody());
    }

    // ── ProxyServletResponse CORS ──────────────────────────────────────────

    @Test
    void aProxyErrorRestoresTheFilterSetCorsValueRustReplaced() {
        for (boolean viaSet : new boolean[] {false, true}) {
            MockHttpServletResponse raw = new MockHttpServletResponse();
            raw.setHeader("Access-Control-Allow-Origin", "https://filter.example");
            raw.setHeader("X-Frame-Options", "DENY");
            ProxyServletResponse response =
                    new ProxyServletResponse(raw, null, null, CorsHandling.RUST);
            if (viaSet) {
                response.setHeader("Access-Control-Allow-Origin", "https://rust.example");
            } else {
                response.addHeader("Access-Control-Allow-Origin", "https://rust.example");
            }
            response.addHeader("X-Rust", "1");
            assertEquals(List.of("https://rust.example"), raw.getHeaders("Access-Control-Allow-Origin"));

            response.resetProxyOutput();

            assertEquals(List.of("https://filter.example"), raw.getHeaders("Access-Control-Allow-Origin"));
            assertEquals("DENY", raw.getHeader("X-Frame-Options"));
            assertNull(raw.getHeader("X-Rust"));
        }
    }

    @Test
    void anAsyncRustCorsValueIsRestoredByAProxyErrorReset() {
        // The ASYNC entity path (dedupeCorsHeaders, RUST) replaces the filter-set
        // value; a later proxy-error reset must restore that value like the
        // addHeader/setHeader paths do.
        MockHttpServletResponse raw = new MockHttpServletResponse();
        raw.setHeader("Access-Control-Allow-Origin", "https://filter.example");
        ProxyServletResponse response = new ProxyServletResponse(raw, null, null, CorsHandling.RUST);
        HttpHeaders rust = new HttpHeaders();
        rust.add("Access-Control-Allow-Origin", "https://rust.example");

        response.dedupeCorsHeaders(rust);
        assertEquals(List.of("https://rust.example"), raw.getHeaders("Access-Control-Allow-Origin"));

        response.resetProxyOutput();

        assertEquals(List.of("https://filter.example"), raw.getHeaders("Access-Control-Allow-Origin"));
    }

    @Test
    void asyncCorsDedupeNeverTouchesAResponseAfterTheAsyncRequestEnded() {
        HttpHeaders rust = new HttpHeaders();
        rust.add("Access-Control-Allow-Origin", "https://rust.example");
        rust.add("Content-Type", "text/plain");

        MockHttpServletResponse open = new MockHttpServletResponse();
        open.setHeader("Access-Control-Allow-Origin", "https://filter.example");
        HttpHeaders kept = new ProxyServletResponse(open, null, null, CorsHandling.RUST).dedupeCorsHeaders(rust);
        assertEquals("https://rust.example", open.getHeader("Access-Control-Allow-Origin"));
        assertNull(kept.getFirst("Access-Control-Allow-Origin"));

        // Timed out / failed / completed: the container owns the response now.
        MockHttpServletResponse closed = new MockHttpServletResponse();
        closed.setHeader("Access-Control-Allow-Origin", "https://filter.example");
        ProxyServletResponse late = new ProxyServletResponse(closed, null, null, CorsHandling.RUST);
        late.closeAsync();
        late.dedupeCorsHeaders(rust);
        assertEquals(List.of("https://filter.example"), closed.getHeaders("Access-Control-Allow-Origin"));
    }
}
