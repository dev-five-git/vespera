package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.LinkedHashMap;
import org.junit.jupiter.api.Test;
import org.springframework.core.io.Resource;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.web.server.ResponseStatusException;

/**
 * B4 (duplicate request-header joining, no longer silently dropped) and
 * P1 (provably-bodyless requests skip the servlet InputStream read).
 */
class ProxyControllerBodyHeaderTest {

    // ── B4: collectHeaders joins repeated header values ──────────────────

    @Test
    void duplicateHeadersAreCommaJoined() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("Accept", "text/html");
        req.addHeader("Accept", "application/json");
        Map<String, String> headers = HeaderPolicy.collectHeaders(req);
        assertEquals("text/html, application/json", headers.get("accept"));
    }

    // ── C-2: async executor backpressure (AbortPolicy → 503) ─────────────

    @Test
    void asyncRejectionMapsTo503AndOtherFailuresPropagate() {
        // CompletableFuture delivers an executor rejection wrapped in a
        // CompletionException. asyncFailureToResponse must turn that into a 503
        // backpressure response (instead of letting the heavy wire build run on
        // a Rust Tokio worker, the CallerRunsPolicy hazard this replaces), while
        // re-propagating every OTHER failure unchanged so Spring maps it as
        // before.
        Throwable rejected = new java.util.concurrent.CompletionException(
                new java.util.concurrent.RejectedExecutionException("queue full"));
        assertTrue(VesperaProxyController.isRejectedExecution(rejected));
        assertFalse(VesperaProxyController.isRejectedExecution(new RuntimeException("boom")));

        ResponseEntity<?> resp = VesperaProxyController.asyncFailureToResponse(rejected);
        assertEquals(503, resp.getStatusCode().value());

        assertThrows(java.util.concurrent.CompletionException.class,
                () -> VesperaProxyController.asyncFailureToResponse(new RuntimeException("boom")));
    }

    // ── ASYNC buffered-response cap parity with SYNC ─────────────────────

    /** Build a wire response {@code [u32 BE headerLen | header JSON | body]}. */
    private static byte[] wireResponseWithBody(int bodyLen) {
        String json =
                "{\"v\":1,\"status\":200,\"headers\":{\"content-type\":\"application/json\"},"
                        + "\"metadata\":{\"version\":\"0.1.0\"}}";
        byte[] hb = json.getBytes(StandardCharsets.UTF_8);
        byte[] body = new byte[bodyLen];
        java.util.Arrays.fill(body, (byte) 'x');
        ByteBuffer buf = ByteBuffer.allocate(4 + hb.length + bodyLen);
        buf.putInt(hb.length);
        buf.put(hb);
        buf.put(body);
        return buf.array();
    }

    @Test
    void asyncResponseEnforcesMaxBufferedResponseCap() {
        // A custom DispatchModeResolver returning ASYNC must honour the same
        // max-buffered-response cap as SYNC (dispatchSync), or it heap-buffers
        // an unbounded Rust response. The capped builder the async flow now
        // uses rejects an oversized body with 413, lets a within-cap body
        // through, and treats cap = 0 as unlimited (never rejects).
        byte[] oversized = wireResponseWithBody(100);
        ResponseStatusException tooLarge = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.buildCappedResponseEntityFromWire(oversized, "GET", 10));
        assertEquals(413, tooLarge.getStatusCode().value());

        byte[] small = wireResponseWithBody(5);
        ResponseEntity<?> ok =
                VesperaProxyController.buildCappedResponseEntityFromWire(small, "GET", 1000);
        assertEquals(200, ok.getStatusCode().value());

        ResponseEntity<?> unlimited =
                VesperaProxyController.buildCappedResponseEntityFromWire(oversized, "GET", 0);
        assertEquals(200, unlimited.getStatusCode().value());
    }

    @Test
    void duplicateCookieHeadersAreSemicolonJoined() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("Cookie", "a=1");
        req.addHeader("Cookie", "b=2");
        Map<String, String> headers = HeaderPolicy.collectHeaders(req);
        // RFC 6265bis: Cookie joins with "; ", never ",".
        assertEquals("a=1; b=2", headers.get("cookie"));
    }

    @Test
    void singleValuedHeaderIsUnchanged() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("X-Trace-Id", "abc123");
        Map<String, String> headers = HeaderPolicy.collectHeaders(req);
        assertEquals("abc123", headers.get("x-trace-id"));
    }

    @Test
    void requestHopByHopAndConnectionNominatedHeadersAreDropped() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.addHeader("Connection", "X-Internal-Hop, x-another-hop");
        req.addHeader("X-Internal-Hop", "secret");
        req.addHeader("X-Another-Hop", "secret2");
        req.addHeader("Transfer-Encoding", "chunked");
        req.addHeader("Content-Type", "application/json");
        req.addHeader("X-Trace-Id", "abc123");

        Map<String, String> headers = HeaderPolicy.collectHeaders(req);

        assertFalse(headers.containsKey("connection"));
        assertFalse(headers.containsKey("x-internal-hop"));
        assertFalse(headers.containsKey("x-another-hop"));
        assertFalse(headers.containsKey("transfer-encoding"));
        assertEquals("application/json", headers.get("content-type"));
        assertEquals("abc123", headers.get("x-trace-id"));
    }

    @Test
    void streamingHeaderFastPathMatchesPreviousMergedMapBytesExactly() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("Connection", "X-Hop");
        req.addHeader("X-Hop", "drop-me");
        req.addHeader("Accept", "text/html");
        req.addHeader("accept", "application/json");
        req.addHeader("Cookie", "a=1");
        req.addHeader("cookie", "b=2");
        req.addHeader("X-Trace-Id", "abc123");

        Map<String, String> previous = previousLinkedHashMapCollect(req);
        byte[] expected = VesperaBridge.encodeRequest(null, "GET", "/x", null, previous, null);
        byte[] actual = VesperaBridge.encodeRequest(null, "GET", "/x", null,
                (VesperaBridge.HeaderSource) sink -> HeaderPolicy.forEachRequestHeader(req, sink),
                null);

        assertArrayEquals(expected, actual);
        assertEquals("text/html, application/json", previous.get("accept"));
        assertEquals("a=1; b=2", previous.get("cookie"));
    }

    private static Map<String, String> previousLinkedHashMapCollect(MockHttpServletRequest req) {
        Map<String, String> merged = new LinkedHashMap<>(32);
        java.util.Enumeration<String> names = req.getHeaderNames();
        java.util.Set<String> connectionTokens = null;
        java.util.Enumeration<String> connections = req.getHeaders("Connection");
        while (connections.hasMoreElements()) {
            connectionTokens = HeaderPolicy.addConnectionTokens(connectionTokens, connections.nextElement());
        }
        while (names.hasMoreElements()) {
            String name = names.nextElement();
            String lowerName = name.toLowerCase(java.util.Locale.ROOT);
            if (!HeaderPolicy.isHopByHopResponseHeader(lowerName)
                    && !HeaderPolicy.isConnectionNominatedHeader(lowerName, connectionTokens)) {
                String value = String.join(
                        lowerName.equals("cookie") ? "; " : ", ",
                        java.util.Collections.list(req.getHeaders(name)));
                merged.merge(lowerName, value, (left, right) ->
                        left + (lowerName.equals("cookie") ? "; " : ", ") + right);
            }
        }
        return merged;
    }

    // ── P1: readBody skips the stream for provably bodyless requests ─────

    @Test
    void bodylessGetWithoutContentLengthReadsEmpty() throws IOException {
        // No Content-Length, no body — definitelyBodyless() is true, so the
        // servlet InputStream is never touched.
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        assertEquals(0, VesperaProxyController.readBody(req).length);
    }

    @Test
    void contentLengthZeroReadsEmpty() throws IOException {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.setContent(new byte[0]);
        assertEquals(0, VesperaProxyController.readBody(req).length);
    }

    @Test
    void postWithBodyIsReadFully() throws IOException {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.setContent("hello".getBytes(StandardCharsets.UTF_8));
        assertEquals(
                "hello",
                new String(VesperaProxyController.readBody(req), StandardCharsets.UTF_8));
    }

    @Test
    void knownLengthOverBufferedCapIsRejected() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.setContent("hello".getBytes(StandardCharsets.UTF_8));

        ResponseStatusException e = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.readBody(req, 4));

        assertEquals(413, e.getStatusCode().value());
    }

    @Test
    void unknownLengthOverBufferedCapIsRejectedAfterCapPlusOneRead() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x") {
            @Override
            public long getContentLengthLong() {
                return -1;
            }
        };
        req.setContent("hello".getBytes(StandardCharsets.UTF_8));

        ResponseStatusException e = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.readBody(req, 4));

        assertEquals(413, e.getStatusCode().value());
    }

    @Test
    void unknownLengthWithHugeConfiguredCapDoesNotAllocateHugeReadBuffer() throws IOException {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x") {
            @Override
            public long getContentLengthLong() {
                return -1;
            }
        };
        req.setContent("hello".getBytes(StandardCharsets.UTF_8));

        byte[] body = VesperaProxyController.readBody(req, Long.MAX_VALUE);

        assertEquals("hello", new String(body, StandardCharsets.UTF_8));
    }

    @Test
    void bufferedCapZeroKeepsBackwardCompatibleUnlimitedRead() throws IOException {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.setContent("hello".getBytes(StandardCharsets.UTF_8));

        assertEquals(5, VesperaProxyController.readBody(req, 0).length);
    }

    @Test
    void configuredBufferedCapRejectsUnknownLengthBodyAfterCapPlusOneRead() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x") {
            @Override
            public long getContentLengthLong() {
                return -1;
            }
        };
        req.setContent(new byte[5]);

        ResponseStatusException e = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.readBody(req, RequestShape.from(req), 4));

        assertEquals(413, e.getStatusCode().value());
    }

    @Test
    void conservativeDefaultBufferedCapRejectsKnownOversizedBodyBeforeRead() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x") {
            @Override
            public long getContentLengthLong() {
                return VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES + 1;
            }
        };

        ResponseStatusException e = assertThrows(
                ResponseStatusException.class,
                () -> VesperaProxyController.readBody(
                        req,
                        RequestShape.from(req),
                        VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES));

        assertEquals(413, e.getStatusCode().value());
    }

    // ── Context-path stripping: Rust sees the context-relative path ──────

    @Test
    void pathWithinApplicationStripsContextPath() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/api/health");
        req.setContextPath("/api");
        req.setRequestURI("/api/health");
        // A non-root deployment must forward `/health`, matching openapi.json.
        assertEquals("/health", VesperaProxyController.pathWithinApplication(req));
    }

    @Test
    void pathWithinApplicationRootContextUnchanged() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/health");
        req.setContextPath("");
        req.setRequestURI("/health");
        assertEquals("/health", VesperaProxyController.pathWithinApplication(req));
    }

    @Test
    void pathWithinApplicationBareContextRootCollapsesToSlash() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/api");
        req.setContextPath("/api");
        req.setRequestURI("/api");
        assertEquals("/", VesperaProxyController.pathWithinApplication(req));
    }

    @Test
    void pathWithinApplicationDoesNotStripPartialSegmentMatch() {
        // Context `/api` must NOT mis-strip a `/apixyz/...` URI.
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/apixyz/foo");
        req.setContextPath("/api");
        req.setRequestURI("/apixyz/foo");
        assertEquals("/apixyz/foo", VesperaProxyController.pathWithinApplication(req));
    }

    @Test
    void directHeaderSynthesizesContentLengthWhenMissing() {
        MockHttpServletResponse response = new MockHttpServletResponse();
        ByteBuffer wire = directWire("{\"status\":200,\"headers\":{}}", "hello");

        int bodyLen = VesperaProxyController.applyDirectHeaderAndPositionBody(wire, response);

        assertEquals(5, bodyLen);
        assertEquals(5, response.getContentLength());
        assertEquals(4 + "{\"status\":200,\"headers\":{}}".getBytes(StandardCharsets.UTF_8).length,
                wire.position());
    }

    @Test
    void directHeaderOwnsContentLengthWhenWireDisagrees() {
        MockHttpServletResponse response = new MockHttpServletResponse();
        ByteBuffer wire = directWire(
                "{\"status\":200,\"headers\":{\"content-length\":\"123\"}}",
                "hello");

        int bodyLen = VesperaProxyController.applyDirectHeaderAndPositionBody(wire, response);

        assertEquals(5, bodyLen);
        assertEquals(5, response.getContentLength());
        assertEquals("5", response.getHeader("Content-Length"));
    }

    @Test
    void directHeaderSuppressesNoBodyStatusBodyAndLength() {
        MockHttpServletResponse response = new MockHttpServletResponse();
        ByteBuffer wire = directWire(
                "{\"status\":204,\"headers\":{\"content-length\":\"123\"}}",
                "hello");

        int bodyLen = VesperaProxyController.applyDirectHeaderAndPositionBody(wire, response);

        assertEquals(0, bodyLen);
        assertEquals(0, response.getContentLength());
        assertEquals("0", response.getHeader("Content-Length"));
    }

    @Test
    void directHeaderSuppressesHeadResponseBody() {
        MockHttpServletResponse response = new MockHttpServletResponse();
        ByteBuffer wire = directWire("{\"status\":200,\"headers\":{}}", "hello");

        int bodyLen = VesperaProxyController.applyDirectHeaderAndPositionBody(
                wire, response, "HEAD");

        assertEquals(0, bodyLen);
        assertEquals(5, response.getContentLength());
    }

    @Test
    void asyncResponseEntityAdvertisesHeadRepresentationLengthAndSuppressesHeadBody() throws IOException {
        byte[] wire = heapWire(
                "{\"status\":200,\"headers\":{\"content-length\":\"123\"}}",
                "hello");

        ResponseEntity<?> entity = VesperaProxyController.buildResponseEntityFromWire(wire, "HEAD");

        assertEquals(5, entity.getHeaders().getContentLength());
        Resource body = (Resource) entity.getBody();
        assertEquals(0, body.contentLength());
        try (InputStream in = body.getInputStream()) {
            assertEquals(-1, in.read());
        }
    }

    @Test
    void responseConnectionNominatedHeadersAreDropped() {
        byte[] wire = heapWire(
                "{\"status\":200,\"headers\":{\"connection\":\"x-internal-hop\"," 
                        + "\"x-internal-hop\":\"secret\",\"x-visible\":\"ok\"}}",
                "hello");

        ResponseEntity<?> entity = VesperaProxyController.buildResponseEntityFromWire(wire, "GET");

        assertFalse(entity.getHeaders().containsKey("connection"));
        assertFalse(entity.getHeaders().containsKey("x-internal-hop"));
        assertEquals("ok", entity.getHeaders().getFirst("x-visible"));
    }

    @Test
    void streamingHeaderDropsContentLengthAndBodyGateSuppressesNoBodyStatus() throws IOException {
        byte[] header = heapWire(
                "{\"status\":304,\"headers\":{\"content-length\":\"123\","
                        + "\"content-type\":\"text/plain\"}}",
                "");
        MockHttpServletResponse response = new MockHttpServletResponse();

        boolean permits = VesperaProxyController.applyDecodedHeader(header, response, "GET");

        assertFalse(permits);
        assertFalse(response.containsHeader("content-length"));
        assertEquals("text/plain", response.getHeader("content-type"));

        ByteArrayOutputStream sink = new ByteArrayOutputStream();
        VesperaProxyController.BodyPermittingOutputStream out =
                new VesperaProxyController.BodyPermittingOutputStream(sink, "GET");
        out.applyPermitsBody(permits);
        out.write("hello".getBytes(StandardCharsets.UTF_8));
        assertEquals(0, sink.size());
    }

    @Test
    void directHeaderDropsHopByHopHeaders() {
        MockHttpServletResponse response = new MockHttpServletResponse();
        // Wire response carries hop-by-hop `transfer-encoding` / `connection`
        // (which desync framing if forwarded) alongside a normal `content-type`.
        ByteBuffer wire = directWire(
                "{\"status\":200,\"headers\":{\"transfer-encoding\":\"chunked\","
                        + "\"connection\":\"keep-alive\",\"content-type\":\"application/json\"}}",
                "hi");

        int bodyLen = VesperaProxyController.applyDirectHeaderAndPositionBody(wire, response);

        // Hop-by-hop headers are owned by the proxy and never forwarded.
        assertFalse(response.containsHeader("transfer-encoding"));
        assertFalse(response.containsHeader("connection"));
        // Normal application headers pass through unchanged.
        assertEquals("application/json", response.getHeader("content-type"));
        // The proxy still synthesises Content-Length from the body.
        assertEquals(2, bodyLen);
        assertEquals(2, response.getContentLength());
    }

    @Test
    void isHopByHopResponseHeaderClassifiesCaseInsensitively() {
        assertTrue(HeaderPolicy.isHopByHopResponseHeader("Transfer-Encoding"));
        assertTrue(HeaderPolicy.isHopByHopResponseHeader("connection"));
        assertTrue(HeaderPolicy.isHopByHopResponseHeader("UPGRADE"));
        // content-length is not hop-by-hop, but addServletResponseHeader treats
        // it as proxy-owned framing and drops it separately.
        assertFalse(HeaderPolicy.isHopByHopResponseHeader("content-length"));
        assertFalse(HeaderPolicy.isHopByHopResponseHeader("content-type"));
    }

    private static ByteBuffer directWire(String headerJson, String body) {
        byte[] header = headerJson.getBytes(StandardCharsets.UTF_8);
        byte[] bodyBytes = body.getBytes(StandardCharsets.UTF_8);
        ByteBuffer buf = ByteBuffer.allocateDirect(4 + header.length + bodyBytes.length);
        buf.putInt(header.length);
        buf.put(header);
        buf.put(bodyBytes);
        buf.flip();
        return buf.asReadOnlyBuffer();
    }

    private static byte[] heapWire(String headerJson, String body) {
        byte[] header = headerJson.getBytes(StandardCharsets.UTF_8);
        byte[] bodyBytes = body.getBytes(StandardCharsets.UTF_8);
        byte[] wire = new byte[4 + header.length + bodyBytes.length];
        wire[0] = (byte) (header.length >>> 24);
        wire[1] = (byte) (header.length >>> 16);
        wire[2] = (byte) (header.length >>> 8);
        wire[3] = (byte) header.length;
        System.arraycopy(header, 0, wire, 4, header.length);
        System.arraycopy(bodyBytes, 0, wire, 4 + header.length, bodyBytes.length);
        return wire;
    }
}
