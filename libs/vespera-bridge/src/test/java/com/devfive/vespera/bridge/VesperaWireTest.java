package com.devfive.vespera.bridge;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Pure-Java tests for the wire encode/decode helpers in
 * {@link VesperaBridge}.  These do NOT load the native library so they
 * run in any JVM.  The native {@code dispatchBytes} symbol is exercised
 * end-to-end via the Rust integration tests and the {@code rust-jni-demo}
 * Spring Boot smoke check.
 */
class VesperaWireTest {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    @Test
    void encodeRequest_layout_starts_with_big_endian_length_prefix() throws Exception {
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", "/x", null, Map.of(), new byte[0]);

        assertTrue(wire.length >= 4, "wire must include length prefix");

        ByteBuffer buf = ByteBuffer.wrap(wire).order(ByteOrder.BIG_ENDIAN);
        int headerLen = buf.getInt();

        assertEquals(wire.length, 4 + headerLen,
                "no body → total length == 4 + headerLen");

        byte[] headerJson = new byte[headerLen];
        System.arraycopy(wire, 4, headerJson, 0, headerLen);
        JsonNode header = MAPPER.readTree(headerJson);

        assertEquals(1, header.path("v").asInt(), "version must be 1");
        assertEquals("GET", header.path("method").asText());
        assertEquals("/x", header.path("path").asText());
    }

    @Test
    void encodeRequest_handles_non_ascii_path() throws Exception {
        byte[] wire = VesperaBridge.encodeRequest(
                "GET", "/한글/path", null, Map.of(), new byte[0]);

        int headerLen = ByteBuffer.wrap(wire).order(ByteOrder.BIG_ENDIAN).getInt();
        byte[] headerJson = new byte[headerLen];
        System.arraycopy(wire, 4, headerJson, 0, headerLen);
        JsonNode header = MAPPER.readTree(headerJson);
        assertEquals("/한글/path", header.path("path").asText());

        // total wire length must equal length prefix + header bytes (no body)
        assertEquals(wire.length, 4 + headerLen);
    }

    @Test
    void encodeRequest_includes_query_and_headers_when_present() throws Exception {
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("content-type", "application/json");
        headers.put("x-trace-id", "abc-123");

        byte[] wire = VesperaBridge.encodeRequest(
                "POST", "/users", "page=1", headers, "{\"x\":1}".getBytes(StandardCharsets.UTF_8));

        int headerLen = ByteBuffer.wrap(wire).order(ByteOrder.BIG_ENDIAN).getInt();
        byte[] headerJson = new byte[headerLen];
        System.arraycopy(wire, 4, headerJson, 0, headerLen);
        JsonNode h = MAPPER.readTree(headerJson);
        assertEquals("page=1", h.path("query").asText());
        assertEquals("application/json", h.path("headers").path("content-type").asText());
        assertEquals("abc-123", h.path("headers").path("x-trace-id").asText());

        // body bytes follow header verbatim
        byte[] body = new byte[wire.length - 4 - headerLen];
        System.arraycopy(wire, 4 + headerLen, body, 0, body.length);
        assertEquals("{\"x\":1}", new String(body, StandardCharsets.UTF_8));
    }

    /** Build a synthetic wire response (mimics what Rust would emit). */
    private static byte[] buildWireResponse(int status, String contentType, byte[] body) throws Exception {
        return buildWireResponseWithExtras(status, contentType, body, null);
    }

    /** Build a synthetic wire response with optional validation_errors header field. */
    private static byte[] buildWireResponseWithExtras(
            int status, String contentType, byte[] body,
            List<Map<String, Object>> validationErrors) throws Exception {
        Map<String, Object> headerMap = new LinkedHashMap<>();
        headerMap.put("v", 1);
        headerMap.put("status", status);
        Map<String, Object> headers = new LinkedHashMap<>();
        if (contentType != null) headers.put("content-type", contentType);
        headerMap.put("headers", headers);
        Map<String, Object> metadata = new LinkedHashMap<>();
        metadata.put("version", "0.1.51");
        headerMap.put("metadata", metadata);
        if (validationErrors != null) {
            headerMap.put("validation_errors", validationErrors);
        }

        byte[] headerJson = MAPPER.writeValueAsBytes(headerMap);
        ByteBuffer buf = ByteBuffer.allocate(4 + headerJson.length + body.length)
                .order(ByteOrder.BIG_ENDIAN);
        buf.putInt(headerJson.length);
        buf.put(headerJson);
        buf.put(body);
        return buf.array();
    }

    @Test
    void decodeResponse_parses_status_headers_and_body() throws Exception {
        byte[] wire = buildWireResponse(
                418, "text/plain; charset=utf-8", "I'm a teapot".getBytes(StandardCharsets.UTF_8));

        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);
        assertEquals(418, decoded.status());
        assertEquals("text/plain; charset=utf-8", decoded.headers().get("content-type"));
        assertEquals("0.1.51", decoded.metadata().get("version"));
        assertEquals("I'm a teapot",
                new String(decoded.body(), StandardCharsets.UTF_8));
    }

    @Test
    void decodeResponse_throws_on_short_input() {
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.decodeResponse(new byte[3]));
        assertTrue(ex.getMessage().contains("too short"), ex.getMessage());
    }

    @Test
    void decodeResponse_throws_when_header_len_overflows() {
        // header_len = Integer.MAX_VALUE but only 4 bytes total
        ByteBuffer buf = ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN);
        buf.putInt(Integer.MAX_VALUE);
        IllegalArgumentException ex = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.decodeResponse(buf.array()));
        assertTrue(ex.getMessage().contains("overflows"), ex.getMessage());
    }

    @Test
    void roundtrip_preserves_binary_body_byte_for_byte() throws Exception {
        byte[] payload = new byte[1024];
        for (int i = 0; i < payload.length; i++) {
            payload[i] = (byte) (i & 0xFF);
        }
        // Use the response-builder to simulate what dispatch would return,
        // then decode it.
        byte[] wire = buildWireResponse(200, "application/octet-stream", payload);
        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);

        assertEquals(200, decoded.status());
        assertArrayEquals(payload, decoded.body(),
                "binary body must round-trip byte-for-byte");
    }

    @Test
    void decodeResponse_hoists_validation_errors_when_present() throws Exception {
        List<Map<String, Object>> errs = new ArrayList<>();
        Map<String, Object> e1 = new LinkedHashMap<>();
        e1.put("path", "username");
        e1.put("code", "length");
        e1.put("message", "too short");
        errs.add(e1);
        Map<String, Object> e2 = new LinkedHashMap<>();
        e2.put("path", "email");
        e2.put("message", "not an email");
        errs.add(e2);

        byte[] wire = buildWireResponseWithExtras(
                422,
                "application/json",
                "{\"errors\":[...]}".getBytes(StandardCharsets.UTF_8),
                errs);

        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);
        assertEquals(422, decoded.status());
        assertNotNull(decoded.validationErrors(),
                "validationErrors should be populated when present in wire");
        assertEquals(2, decoded.validationErrors().size());
        assertEquals("username", decoded.validationErrors().get(0).get("path"));
        assertEquals("length", decoded.validationErrors().get(0).get("code"));
        assertEquals("too short", decoded.validationErrors().get(0).get("message"));
        assertEquals("email", decoded.validationErrors().get(1).get("path"));
        // Body still preserved alongside the hoisted header field:
        assertArrayEquals(
                "{\"errors\":[...]}".getBytes(StandardCharsets.UTF_8),
                decoded.body(),
                "body must be preserved verbatim even when errors are hoisted");
    }

    @Test
    void decodeResponse_validation_errors_null_when_absent() throws Exception {
        // Non-422 response should have null validationErrors
        byte[] wire = buildWireResponse(200, "text/plain", "ok".getBytes(StandardCharsets.UTF_8));
        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);
        assertEquals(200, decoded.status());
        assertNull(decoded.validationErrors(),
                "non-422 response must not carry validationErrors");
    }

    @Test
    void encode_decode_full_request_roundtrip_via_synthetic_response() throws Exception {
        // Build a request, "echo" it back as the response body, decode.
        byte[] reqBody = "hello".getBytes(StandardCharsets.UTF_8);
        byte[] reqWire = VesperaBridge.encodeRequest(
                "POST", "/echo", null,
                Map.of("content-type", "text/plain"),
                reqBody);

        // Extract body from request, mirror it back in a response wire.
        int reqHeaderLen = ByteBuffer.wrap(reqWire).order(ByteOrder.BIG_ENDIAN).getInt();
        byte[] echoedBody = new byte[reqWire.length - 4 - reqHeaderLen];
        System.arraycopy(reqWire, 4 + reqHeaderLen, echoedBody, 0, echoedBody.length);

        byte[] respWire = buildWireResponse(200, "text/plain", echoedBody);
        DecodedResponse decoded = VesperaBridge.decodeResponse(respWire);

        assertArrayEquals(reqBody, decoded.body());
    }
}
