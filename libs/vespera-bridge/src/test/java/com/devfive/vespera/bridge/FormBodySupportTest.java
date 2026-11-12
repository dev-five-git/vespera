package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertTrue;

import jakarta.servlet.ServletInputStream;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletRequestWrapper;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.DelegatingServletInputStream;
import org.springframework.mock.web.MockHttpServletRequest;

/**
 * Form-urlencoded bodies consumed by Spring's {@code FormContentFilter}
 * (PUT/PATCH/DELETE) or an earlier {@code getParameter} call (POST) are
 * rebuilt from the request parameters instead of reaching Rust empty.
 */
class FormBodySupportTest {

    private static final String FORM = "application/x-www-form-urlencoded";

    /**
     * A request whose body stream was drained by a filter while the original
     * {@code Content-Length} header (and the parsed parameters) survive —
     * exactly what the proxy sees behind {@code FormContentFilter}.
     */
    private static HttpServletRequest consumed(MockHttpServletRequest req, long declaredLength) {
        return new HttpServletRequestWrapper(req) {
            @Override
            public long getContentLengthLong() {
                return declaredLength;
            }

            @Override
            public int getContentLength() {
                return (int) declaredLength;
            }

            @Override
            public ServletInputStream getInputStream() {
                return new DelegatingServletInputStream(new ByteArrayInputStream(new byte[0]));
            }
        };
    }

    private static MockHttpServletRequest form(String method) {
        MockHttpServletRequest req = new MockHttpServletRequest(method, "/form");
        req.setContentType(FORM);
        return req;
    }

    private static String text(byte[] bytes) {
        return new String(bytes, StandardCharsets.US_ASCII);
    }

    @Test
    void consumedPatchBodyIsRebuiltFromParameters() throws IOException {
        MockHttpServletRequest req = form("PATCH");
        req.setCharacterEncoding("UTF-8");
        req.addParameter("name", "Jane Doe");
        req.addParameter("tag", "a", "b&c");
        req.addParameter("city", "Séoul");
        HttpServletRequest request = consumed(req, 52);

        byte[] body = VesperaProxyController.readBody(request, 0);

        assertEquals("name=Jane+Doe&tag=a&tag=b%26c&city=S%C3%A9oul", text(body));
    }

    @Test
    void queryStringParametersAreNotCopiedIntoTheBody() throws IOException {
        MockHttpServletRequest req = form("POST");
        req.setQueryString("page=2&tag=q&empty&&bad%zz=1");
        // Containers list query values first for a shared name.
        req.addParameter("page", "2");
        req.addParameter("tag", "q", "body-tag");
        req.addParameter("empty", "");
        req.addParameter("bad%zz", "1");
        req.addParameter("note", "hi");
        HttpServletRequest request = consumed(req, 25);

        byte[] body = VesperaProxyController.readBody(request, 0);

        assertEquals("tag=body-tag&note=hi", text(body));
    }

    @Test
    void withoutCharacterEncodingEachDecodersDefaultCharsetIsUsed() {
        // POST: parsed by the container, servlet default ISO-8859-1.
        MockHttpServletRequest post = form("POST");
        post.addParameter("city", "Séoul");
        assertEquals("city=S%E9oul", text(FormBodySupport.rebuildFromParameters(post)));

        // PUT/PATCH/DELETE: parsed by FormContentFilter, UTF-8 by default.
        MockHttpServletRequest put = form("PUT");
        put.addParameter("city", "Séoul");
        assertEquals("city=S%C3%A9oul", text(FormBodySupport.rebuildFromParameters(put)));
    }

    @Test
    void unsupportedCharacterEncodingFallsBackToUtf8() {
        MockHttpServletRequest req = form("PUT");
        req.setCharacterEncoding("x-no-such-charset");
        assertEquals(StandardCharsets.UTF_8, FormBodySupport.requestCharset(req));
    }

    @Test
    void intactBodiesAndNonFormBodiesAreLeftAlone() {
        MockHttpServletRequest req = form("POST");
        req.addParameter("a", "1");
        byte[] full = "a=1".getBytes(StandardCharsets.US_ASCII);
        assertSame(full, FormBodySupport.recoverConsumedBody(req, full, 3));
        // Unknown length with bytes present: not a short read.
        assertSame(full, FormBodySupport.recoverConsumedBody(req, full, -1));

        MockHttpServletRequest json = new MockHttpServletRequest("POST", "/json");
        json.setContentType("application/json");
        json.addParameter("a", "1");
        byte[] empty = new byte[0];
        assertSame(empty, FormBodySupport.recoverConsumedBody(json, empty, 10));
    }

    @Test
    void shortBodyWithoutBodyParametersKeepsTheOriginalBytes() {
        MockHttpServletRequest req = form("POST");
        req.setQueryString("only=query");
        req.addParameter("only", "query");
        byte[] partial = "x".getBytes(StandardCharsets.US_ASCII);

        assertSame(partial, FormBodySupport.recoverConsumedBody(req, partial, 10));
    }

    @Test
    void formContentTypeDetectionIgnoresCaseParametersAndWhitespace() {
        MockHttpServletRequest req = new MockHttpServletRequest("POST", "/");
        assertFalse(FormBodySupport.isFormUrlEncoded(req));
        req.setContentType("  Application/X-WWW-Form-Urlencoded; charset=UTF-8");
        assertTrue(FormBodySupport.isFormUrlEncoded(req));
        req.setContentType("multipart/form-data; boundary=x");
        assertFalse(FormBodySupport.isFormUrlEncoded(req));
    }

    @Test
    void queryParameterCountsDecodeNamesAndSkipEmptyPairs() {
        Map<String, Integer> expected = new LinkedHashMap<>();
        expected.put("a b", 2);
        expected.put("c", 1);
        expected.put("bad%zz", 1);
        assertEquals(expected, FormBodySupport.queryParameterCounts("a+b=1&&a%20b=2&c&bad%zz=1"));
        assertTrue(FormBodySupport.queryParameterCounts(null).isEmpty());
        assertTrue(FormBodySupport.queryParameterCounts("").isEmpty());
    }

    @Test
    void bidirectionalFormRequestsBufferTheRequestBody() {
        MockHttpServletRequest req = form("POST");
        assertEquals(DispatchMode.STREAMING,
                VesperaProxyController.formSafeMode(DispatchMode.BIDIRECTIONAL_STREAMING, req));
        assertEquals(DispatchMode.SYNC, VesperaProxyController.formSafeMode(DispatchMode.SYNC, req));

        MockHttpServletRequest upload = new MockHttpServletRequest("POST", "/upload");
        upload.setContentType("application/octet-stream");
        assertEquals(DispatchMode.BIDIRECTIONAL_STREAMING,
                VesperaProxyController.formSafeMode(DispatchMode.BIDIRECTIONAL_STREAMING, upload));
    }

    @Test
    void rebuiltBodyForwardsItsOwnContentLength() {
        MockHttpServletRequest req = form("PATCH");
        req.setContent("a=1".getBytes(StandardCharsets.US_ASCII));
        RequestShape shape = RequestShape.from(consumed(req, 40));
        assertEquals("3", VesperaProxyController.forwardedContentLength(shape, new byte[3]));

        MockHttpServletRequest same = form("PATCH");
        same.setContent("a=1".getBytes(StandardCharsets.US_ASCII));
        assertNull(VesperaProxyController.forwardedContentLength(RequestShape.from(same), new byte[3]));

        MockHttpServletRequest chunked = form("PATCH");
        chunked.addHeader("Transfer-Encoding", "chunked");
        assertNull(VesperaProxyController.forwardedContentLength(RequestShape.from(chunked), new byte[3]));
    }

    @Test
    void rebuiltBodyMatchesWhatTheFilterParsed() throws IOException {
        // Round trip: the rebuilt bytes decode back to the same body parameters.
        MockHttpServletRequest req = form("DELETE");
        req.setCharacterEncoding("UTF-8");
        req.addParameter("reason", "100% sure & done");
        byte[] body = VesperaProxyController.readBody(consumed(req, 30), 0);
        assertArrayEquals("reason=100%25+sure+%26+done".getBytes(StandardCharsets.US_ASCII), body);
    }
}
