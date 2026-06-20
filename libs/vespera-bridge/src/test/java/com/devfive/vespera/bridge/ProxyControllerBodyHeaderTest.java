package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import org.junit.jupiter.api.Test;
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
        Map<String, String> headers = VesperaProxyController.collectHeaders(req);
        assertEquals("text/html, application/json", headers.get("accept"));
    }

    @Test
    void duplicateCookieHeadersAreSemicolonJoined() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("Cookie", "a=1");
        req.addHeader("Cookie", "b=2");
        Map<String, String> headers = VesperaProxyController.collectHeaders(req);
        // RFC 6265bis: Cookie joins with "; ", never ",".
        assertEquals("a=1; b=2", headers.get("cookie"));
    }

    @Test
    void singleValuedHeaderIsUnchanged() {
        MockHttpServletRequest req = new MockHttpServletRequest("GET", "/x");
        req.addHeader("X-Trace-Id", "abc123");
        Map<String, String> headers = VesperaProxyController.collectHeaders(req);
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
    void directHeaderPreservesWireContentLength() {
        MockHttpServletResponse response = new MockHttpServletResponse();
        ByteBuffer wire = directWire(
                "{\"status\":200,\"headers\":{\"content-length\":\"123\"}}",
                "hello");

        int bodyLen = VesperaProxyController.applyDirectHeaderAndPositionBody(wire, response);

        assertEquals(5, bodyLen);
        assertEquals(123, response.getContentLength());
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
        assertTrue(VesperaProxyController.isHopByHopResponseHeader("Transfer-Encoding"));
        assertTrue(VesperaProxyController.isHopByHopResponseHeader("connection"));
        assertTrue(VesperaProxyController.isHopByHopResponseHeader("UPGRADE"));
        // content-length is deliberately preserved (handler-authoritative).
        assertFalse(VesperaProxyController.isHopByHopResponseHeader("content-length"));
        assertFalse(VesperaProxyController.isHopByHopResponseHeader("content-type"));
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
}
