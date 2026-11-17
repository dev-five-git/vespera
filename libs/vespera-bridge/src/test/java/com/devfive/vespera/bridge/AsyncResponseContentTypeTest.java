package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.asyncDispatch;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;

import jakarta.servlet.http.HttpServletRequest;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.CsvSource;
import org.junit.jupiter.params.provider.ValueSource;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.test.web.servlet.MockMvc;
import org.springframework.test.web.servlet.MvcResult;
import org.springframework.test.web.servlet.setup.MockMvcBuilders;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.context.request.async.DeferredResult;

/**
 * ASYNC responses whose Rust handler set no {@code Content-Type} go out
 * without one, like SYNC / DIRECT: Spring's content negotiation must not
 * invent one from {@code Accept} (a browser would get {@code text/html}).
 * A Rust {@code Content-Type} and the proxy's own error entities keep theirs.
 */
class AsyncResponseContentTypeTest {

    private static byte[] wire(String headers, String body) {
        byte[] h = ("{\"v\":1,\"status\":200,\"headers\":" + headers + ",\"metadata\":{}}")
                .getBytes(StandardCharsets.UTF_8);
        byte[] b = body.getBytes(StandardCharsets.UTF_8);
        return ByteBuffer.allocate(4 + h.length + b.length).putInt(h.length).put(h).put(b).array();
    }

    /** Mirrors the proxy: an {@code Object} handler answering through a {@link DeferredResult}. */
    @RestController
    static class AsyncController {
        @GetMapping("/{kind}")
        Object proxy(HttpServletRequest request, @org.springframework.web.bind.annotation.PathVariable("kind") String kind) {
            VesperaAsyncResponseBodyAdvice.markProxied(request);
            DeferredResult<Object> result = new DeferredResult<>();
            switch (kind) {
                case "bare" -> result.setResult(WireResponseWriter.buildAsyncResponseEntityFromWire(
                        wire("{}", "raw body"), "GET", null));
                case "empty" -> result.setResult(WireResponseWriter.buildAsyncResponseEntityFromWire(
                        wire("{}", ""), "GET", null));
                case "typed" -> result.setResult(WireResponseWriter.buildAsyncResponseEntityFromWire(
                        wire("{\"content-type\":\"application/json\"}", "{}"), "GET", null));
                default -> result.setResult(AsyncProxyResponses.asyncFailureToResponse(
                        new RuntimeException("boom"), null));
            }
            return result;
        }

        /** A Rust Content-Type Spring cannot parse; {@code status} 204 has no body. */
        @org.springframework.web.bind.annotation.RequestMapping(
                value = "/raw/{status}",
                method = {org.springframework.web.bind.annotation.RequestMethod.GET,
                        org.springframework.web.bind.annotation.RequestMethod.POST})
        Object raw(HttpServletRequest request, jakarta.servlet.http.HttpServletResponse servletResponse,
                @org.springframework.web.bind.annotation.PathVariable("status") int status) {
            ProxyServletResponse proxied = new ProxyServletResponse(servletResponse, null, null);
            VesperaAsyncResponseBodyAdvice.markProxied(request, proxied);
            byte[] h = ("{\"v\":1,\"status\":" + status + ",\"headers\":{\"content-type\":\"json\","
                    + "\"etag\":\"\\\"v1\\\"\"},\"metadata\":{}}").getBytes(StandardCharsets.UTF_8);
            byte[] b = "{}".getBytes(StandardCharsets.UTF_8);
            byte[] w = ByteBuffer.allocate(4 + h.length + b.length).putInt(h.length).put(h).put(b).array();
            DeferredResult<Object> result = new DeferredResult<>();
            result.setResult(WireResponseWriter.buildAsyncResponseEntityFromWire(w, request.getMethod(), proxied));
            return result;
        }

        /** As the proxy: marked with its per-request {@link ProxyServletResponse}. */
        @org.springframework.web.bind.annotation.RequestMapping(
                value = "/files/{name}",
                method = {org.springframework.web.bind.annotation.RequestMethod.GET,
                        org.springframework.web.bind.annotation.RequestMethod.HEAD})
        Object file(HttpServletRequest request, jakarta.servlet.http.HttpServletResponse servletResponse,
                @org.springframework.web.bind.annotation.PathVariable("name") String name) {
            ProxyServletResponse proxied = new ProxyServletResponse(servletResponse, null, null);
            VesperaAsyncResponseBodyAdvice.markProxied(request, proxied);
            DeferredResult<Object> result = new DeferredResult<>();
            result.setResult(WireResponseWriter.buildAsyncResponseEntityFromWire(
                    wire("{\"content-type\":\"" + rustContentType(name) + "\",\"etag\":\"\\\"v1\\\"\","
                            + "\"last-modified\":\"Wed, 21 Oct 2015 07:28:00 GMT\"}", "%PDF-1.7"),
                    request.getMethod(), proxied));
            return result;
        }

        /** The type Rust declares for the file: the extension decides, so each case differs. */
        private static String rustContentType(String name) {
            if (name.endsWith(".pdf")) {
                return "application/pdf";
            }
            return name.endsWith(".bat") ? "application/x-msdownload" : "text/html";
        }
    }

    private static MockHttpServletResponse call(
            org.springframework.test.web.servlet.request.MockHttpServletRequestBuilder request) throws Exception {
        MockMvc mvc = MockMvcBuilders.standaloneSetup(new AsyncController())
                .setControllerAdvice(new VesperaAsyncResponseBodyAdvice())
                .build();
        MvcResult started = mvc.perform(request).andReturn();
        assertTrue(started.getRequest().isAsyncStarted());
        return mvc.perform(asyncDispatch(started)).andReturn().getResponse();
    }

    /**
     * No converter runs, so Spring's RFD guard never adds Content-Disposition,
     * whatever the extension. The Rust type differs per case (see
     * {@code AsyncController#rustContentType}), so each row checks its own.
     */
    @ParameterizedTest
    @CsvSource({
        "/files/report.pdf, application/pdf",
        "/files/report.bat, application/x-msdownload",
        "/files/x.html;a=b, text/html"})
    void typedBodiesGetNoContentDisposition(String path, String expectedType) throws Exception {
        MockHttpServletResponse response = call(get(path));
        assertEquals(200, response.getStatus());
        assertNull(response.getHeader("Content-Disposition"));
        assertEquals(expectedType, response.getContentType());
        assertEquals("%PDF-1.7", response.getContentAsString());
    }

    /** Rust owns conditional requests: a Rust 200 is never turned into Spring's 304 / 412. */
    @ParameterizedTest
    @ValueSource(strings = {"GET", "HEAD"})
    void conditionalRequestsDoNotTurnARust200Into304(String method) throws Exception {
        MockHttpServletResponse response = call(
                org.springframework.test.web.servlet.request.MockMvcRequestBuilders.request(
                                org.springframework.http.HttpMethod.valueOf(method), "/files/report.pdf")
                        .header("If-None-Match", "\"v1\"")
                        .header("If-Modified-Since", "Wed, 21 Oct 2015 07:28:00 GMT")
                        .header("If-Unmodified-Since", "Tue, 20 Oct 2015 07:28:00 GMT"));
        assertEquals(200, response.getStatus());
        assertEquals("\"v1\"", response.getHeader("ETag"));
        assertEquals("Wed, 21 Oct 2015 07:28:00 GMT", response.getHeader("Last-Modified"));
        assertEquals(method.equals("GET") ? "%PDF-1.7" : "", response.getContentAsString());
    }

    private static MockHttpServletResponse call(String path) throws Exception {
        MockMvc mvc = MockMvcBuilders.standaloneSetup(new AsyncController())
                .setControllerAdvice(new VesperaAsyncResponseBodyAdvice())
                .build();
        MvcResult started = mvc.perform(get(path)
                .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"))
                .andReturn();
        assertTrue(started.getRequest().isAsyncStarted());
        return mvc.perform(asyncDispatch(started)).andReturn().getResponse();
    }

    @ParameterizedTest
    @ValueSource(strings = {"/bare", "/empty"})
    void noRustContentTypeMeansNoContentType(String path) throws Exception {
        MockHttpServletResponse response = call(path);
        assertEquals(200, response.getStatus());
        assertNull(response.getContentType(), "no Content-Type invented from Accept");
        assertNull(response.getHeader("Content-Type"));
        assertEquals(path.equals("/bare") ? "raw body" : "", response.getContentAsString());
    }

    /** SYNC / DIRECT parity: an unparsable Rust Content-Type passes through instead of a Spring 500. */
    @ParameterizedTest
    @ValueSource(strings = {"GET", "POST"})
    void unparsableRustContentTypePassesThrough(String method) throws Exception {
        MockHttpServletResponse response = call(
                org.springframework.test.web.servlet.request.MockMvcRequestBuilders.request(
                        org.springframework.http.HttpMethod.valueOf(method), "/raw/201"));
        assertEquals(201, response.getStatus());
        assertEquals("json", response.getHeader("Content-Type"));
        assertEquals("\"v1\"", response.getHeader("ETag"));
        assertEquals("{}", response.getContentAsString());

        MockHttpServletResponse noBody = call(get("/raw/204"));
        assertEquals(204, noBody.getStatus());
        assertNull(noBody.getHeader("Content-Type"), "dropped: a 204 has no body to describe");
        assertTrue(WireResponseWriter.parsableContentType(null));
        assertTrue(WireResponseWriter.parsableContentType("text/plain; charset=utf-8"));
    }

    @ParameterizedTest
    @ValueSource(strings = {"/typed", "/error"})
    void explicitContentTypesAreKept(String path) throws Exception {
        MockHttpServletResponse response = call(path);
        assertEquals(path.equals("/typed") ? "application/json" : "text/plain;charset=utf-8",
                response.getContentType());
    }
}
