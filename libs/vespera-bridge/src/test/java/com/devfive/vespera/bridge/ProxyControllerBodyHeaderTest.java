package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

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
    void bufferedCapZeroKeepsBackwardCompatibleUnlimitedRead() throws IOException {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/x");
        req.setContent("hello".getBytes(StandardCharsets.UTF_8));

        assertEquals(5, VesperaProxyController.readBody(req, 0).length);
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
