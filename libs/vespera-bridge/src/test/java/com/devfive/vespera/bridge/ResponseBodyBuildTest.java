package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Random;
import org.junit.jupiter.api.Test;
import org.springframework.core.io.InputStreamResource;
import org.springframework.http.HttpHeaders;
import org.springframework.http.ResponseEntity;
import org.springframework.mock.web.MockHttpServletResponse;

/**
 * The buffered response builders write the body straight from the wire
 * buffer and the status / headers through {@link WireHeaderReader}: their
 * output must equal what the full {@link VesperaBridge#decodeResponse} decode
 * says the wire carries, byte for byte, across the content-type / charset
 * matrix (text is never decoded and re-encoded).
 */
class ResponseBodyBuildTest {

    /** Assemble a wire response {@code [u32 len | header | body]}. */
    private static byte[] wire(int status, String headersJson, byte[] body) {
        String header = "{\"v\":1,\"status\":" + status + ",\"headers\":" + headersJson
                + ",\"metadata\":{\"version\":\"0.0.0\"}}";
        byte[] hb = header.getBytes(StandardCharsets.UTF_8);
        byte[] w = new byte[4 + hb.length + body.length];
        w[0] = (byte) (hb.length >>> 24);
        w[1] = (byte) (hb.length >>> 16);
        w[2] = (byte) (hb.length >>> 8);
        w[3] = (byte) hb.length;
        System.arraycopy(hb, 0, w, 4, hb.length);
        System.arraycopy(body, 0, w, 4 + hb.length, body.length);
        return w;
    }

    private static byte[] wire(String contentType, byte[] body) {
        return wire(200, contentType == null ? "{}" : "{\"content-type\":\"" + contentType + "\"}", body);
    }

    private static byte[] asyncBody(ResponseEntity<?> entity) throws IOException {
        Object body = entity.getBody();
        if (body instanceof byte[] bytes) {
            return bytes;
        }
        try (InputStream in = assertInstanceOf(InputStreamResource.class, body).getInputStream()) {
            return in.readAllBytes();
        }
    }

    /** Every buffered builder emits exactly the wire body and the decoded status / headers. */
    private static void assertBuildersMatchDecode(String contentType, byte[] body) throws IOException {
        byte[] w = wire(contentType, body);
        DecodedResponse decoded = VesperaBridge.decodeResponse(w);
        assertArrayEquals(body, decoded.bodyBytes(), "fixture");

        // The capped builder the controller's ASYNC flow calls (cap 0 = unlimited).
        ResponseEntity<?> entity = VesperaProxyController.buildCappedAsyncResponseEntity(w, "GET", 0, null);
        assertEquals(decoded.status(), entity.getStatusCode().value());
        assertArrayEquals(body, asyncBody(entity));
        assertEquals(contentType, entity.getHeaders().getFirst(HttpHeaders.CONTENT_TYPE));
        assertEquals(body.length, entity.getHeaders().getContentLength());

        ResponseEntity<?> async = WireResponseWriter.buildAsyncResponseEntityFromWire(w, "GET", null);
        assertEquals(decoded.status(), async.getStatusCode().value());
        assertArrayEquals(body, asyncBody(async));
        assertEquals(contentType, async.getHeaders().getFirst(HttpHeaders.CONTENT_TYPE));

        MockHttpServletResponse sync = new MockHttpServletResponse();
        WireResponseWriter.writeWireResponse(w, sync, "GET");
        assertEquals(decoded.status(), sync.getStatus());
        assertArrayEquals(body, sync.getContentAsByteArray());
        assertEquals(contentType, sync.getHeader(HttpHeaders.CONTENT_TYPE));
        assertEquals(String.valueOf(body.length), sync.getHeader(HttpHeaders.CONTENT_LENGTH));
    }

    @Test
    void textBodiesAreByteIdentical() throws IOException {
        assertBuildersMatchDecode("application/json", "{\"ok\":true}".getBytes(StandardCharsets.UTF_8));
        assertBuildersMatchDecode("text/plain", "plain ascii".getBytes(StandardCharsets.UTF_8));
        assertBuildersMatchDecode("text/plain; charset=utf-8",
                "café — naïve — 日本語".getBytes(StandardCharsets.UTF_8));
        // 4-byte code point: the multi-byte boundary case.
        assertBuildersMatchDecode("application/json", "ok🚀end".getBytes(StandardCharsets.UTF_8));
        // ISO-8859-1 bytes are not valid UTF-8: a decode/re-encode would mangle them.
        assertBuildersMatchDecode("text/plain; charset=ISO-8859-1", new byte[] {(byte) 0xE9, (byte) 0xE8, 'a'});
    }

    @Test
    void binaryBodiesAreByteIdentical() throws IOException {
        byte[] allBytes = new byte[256];
        for (int i = 0; i < 256; i++) {
            allBytes[i] = (byte) i;
        }
        assertBuildersMatchDecode("application/octet-stream", allBytes);
        byte[] big = new byte[64 * 1024];
        new Random(7).nextBytes(big);
        assertBuildersMatchDecode("application/octet-stream", big);
    }

    @Test
    void emptyBodies() throws IOException {
        assertBuildersMatchDecode("application/json", new byte[0]);
        assertBuildersMatchDecode(null, new byte[0]);
    }

    @Test
    void multiValuedHeadersAndStatusMatchTheFullDecode() throws IOException {
        byte[] w = wire(201, "{\"set-cookie\":[\"a=1\",\"b=2\"],\"content-type\":\"application/json\","
                + "\"x-custom\":\"v\"}", "{}".getBytes(StandardCharsets.UTF_8));
        DecodedResponse decoded = VesperaBridge.decodeResponse(w);
        assertEquals(List.of("a=1", "b=2"), decoded.headers().get("set-cookie"));

        for (ResponseEntity<?> entity : List.of(
                VesperaProxyController.buildCappedAsyncResponseEntity(w, "POST", 1024, null),
                WireResponseWriter.buildAsyncResponseEntityFromWire(w, "POST", null))) {
            assertEquals(201, entity.getStatusCode().value());
            // getValuesAsList: HttpHeaders.get(String) is Spring-7-only (a Map in 6).
            assertEquals(List.of("a=1", "b=2"), entity.getHeaders().getValuesAsList("set-cookie"));
            assertEquals("v", entity.getHeaders().getFirst("x-custom"));
            assertEquals("application/json", entity.getHeaders().getFirst(HttpHeaders.CONTENT_TYPE));
            assertArrayEquals("{}".getBytes(StandardCharsets.UTF_8), asyncBody(entity));
        }

        MockHttpServletResponse sync = new MockHttpServletResponse();
        WireResponseWriter.writeWireResponse(w, sync, "POST");
        assertEquals(201, sync.getStatus());
        assertEquals(List.of("a=1", "b=2"), sync.getHeaders("set-cookie"));
        assertEquals("v", sync.getHeader("x-custom"));
    }
}
