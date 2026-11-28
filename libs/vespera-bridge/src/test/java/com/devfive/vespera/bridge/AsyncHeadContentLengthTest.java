package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.asyncDispatch;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;
import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.request;

import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletResponse;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.http.HttpMethod;
import org.springframework.mock.web.MockHttpServletResponse;
import org.springframework.test.web.servlet.MockMvc;
import org.springframework.test.web.servlet.MvcResult;
import org.springframework.test.web.servlet.request.MockHttpServletRequestBuilder;
import org.springframework.test.web.servlet.setup.MockMvcBuilders;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestMethod;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.context.request.async.DeferredResult;

/**
 * An ASYNC proxied response to HEAD (or GET) with a validator and no
 * {@code content-length} from Rust: the entity must not make Spring add
 * {@code Content-Length: 0}. Rust declared no length, so none is the truth;
 * SYNC / DIRECT send none either. A GET whose Rust body really is empty
 * keeps its {@code Content-Length: 0}.
 */
class AsyncHeadContentLengthTest {

    private static final String HEADERS =
            "{\"v\":1,\"status\":200,\"headers\":{\"content-type\":\"text/plain\","
                    + "\"etag\":\"\\\"v1\\\"\",\"last-modified\":\"Wed, 21 Oct 2015 07:28:00 GMT\"},"
                    + "\"metadata\":{}}";

    /** The wire the axum HEAD handler produces (no body region) or a GET with an empty body. */
    private static byte[] wire(boolean withBody) {
        byte[] header = HEADERS.getBytes(StandardCharsets.UTF_8);
        byte[] body = withBody ? "hello".getBytes(StandardCharsets.UTF_8) : new byte[0];
        return ByteBuffer.allocate(4 + header.length + body.length)
                .putInt(header.length).put(header).put(body).array();
    }

    @RestController
    static class ValidatorController {
        @RequestMapping(value = "/doc/{kind}", method = {RequestMethod.GET, RequestMethod.HEAD})
        Object doc(HttpServletRequest request, HttpServletResponse servletResponse,
                @PathVariable("kind") String kind) {
            ProxyServletResponse proxied = new ProxyServletResponse(servletResponse, null, null);
            VesperaAsyncResponseBodyAdvice.markProxied(request, proxied);
            DeferredResult<Object> result = new DeferredResult<>();
            result.setResult(WireResponseWriter.buildAsyncResponseEntityFromWire(
                    wire(kind.equals("body")), request.getMethod(), proxied));
            return result;
        }
    }

    private static MockHttpServletResponse call(MockHttpServletRequestBuilder builder) throws Exception {
        MockMvc mvc = MockMvcBuilders.standaloneSetup(new ValidatorController())
                .setControllerAdvice(new VesperaAsyncResponseBodyAdvice())
                .build();
        MvcResult started = mvc.perform(builder).andReturn();
        assertTrue(started.getRequest().isAsyncStarted());
        return mvc.perform(asyncDispatch(started)).andReturn().getResponse();
    }

    @Test
    void headWithValidatorsAndNoRustLengthSendsNoContentLength() throws Exception {
        MockHttpServletResponse response = call(request(HttpMethod.HEAD, "/doc/bare"));
        assertEquals(200, response.getStatus());
        assertEquals("\"v1\"", response.getHeader("ETag"));
        assertNull(response.getHeader("Content-Length"), "unknown length is not 0");
        assertEquals("", response.getContentAsString());
    }

    @Test
    void getWithTheSameValidatorsKeepsItsBody() throws Exception {
        MockHttpServletResponse response = call(get("/doc/body"));
        assertEquals(200, response.getStatus());
        assertEquals("\"v1\"", response.getHeader("ETag"));
        assertEquals("5", response.getHeader("Content-Length"));
        assertEquals("hello", response.getContentAsString());
    }
}
