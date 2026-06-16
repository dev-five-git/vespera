package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;

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
}
