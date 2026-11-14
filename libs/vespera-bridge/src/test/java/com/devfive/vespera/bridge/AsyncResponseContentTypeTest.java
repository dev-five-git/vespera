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
                default -> result.setResult(VesperaProxyController.asyncFailureToResponse(
                        new RuntimeException("boom"), null));
            }
            return result;
        }
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

    @ParameterizedTest
    @ValueSource(strings = {"/typed", "/error"})
    void explicitContentTypesAreKept(String path) throws Exception {
        MockHttpServletResponse response = call(path);
        assertEquals(path.equals("/typed") ? "application/json" : "text/plain;charset=utf-8",
                response.getContentType());
    }
}
