package com.devfive.vespera.bridge;

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
