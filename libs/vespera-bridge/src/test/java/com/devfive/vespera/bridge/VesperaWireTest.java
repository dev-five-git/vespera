package com.devfive.vespera.bridge;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
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
        // The query is folded into the `path` field (the full request target)
        // — there is no separate `query` field — so the Rust dispatch side
        // borrows it for `Uri` parsing instead of re-joining `path+'?'+query`.
        assertEquals("/users?page=1", h.path("path").asText());
        assertEquals("", h.path("query").asText());
        assertEquals("application/json", h.path("headers").path("content-type").asText());
        assertEquals("abc-123", h.path("headers").path("x-trace-id").asText());

        // body bytes follow header verbatim
        byte[] body = new byte[wire.length - 4 - headerLen];
        System.arraycopy(wire, 4 + headerLen, body, 0, body.length);
        assertEquals("{\"x\":1}", new String(body, StandardCharsets.UTF_8));
    }

    /**
     * Canonical request header JSON — the <strong>shared cross-language
     * golden</strong> that locks the Java encoder against the Rust
     * {@code serde_json}/hand-rolled parser. The Rust counterpart
     * ({@code crates/vespera_inprocess/tests/wire_contract.rs ::
     * cross_language_request_golden_routes}) dispatches the byte-identical
     * frame and asserts it routes, so the two independent hand-rolled wire
     * implementations cannot silently drift: a change to either side's field
     * order / escaping / structure breaks its own golden assertion.
     *
     * <p>Field order is fixed by {@code VesperaWireCodec.fillHeaderJson}:
     * {@code v, method, path, headers?, app?}.  The query string is folded into
     * {@code path} as the full request target ({@code /users?page=1}) — there
     * is no separate {@code query} field — so the Rust dispatch side borrows the
     * target directly instead of re-joining it (see {@code wire_contract.rs}).
     */
    static final String CANONICAL_REQUEST_HEADER_JSON =
            "{\"v\":1,\"method\":\"POST\",\"path\":\"/users?page=1\","
                    + "\"headers\":{\"content-type\":\"application/json\"}}";

    /** Canonical request body paired with {@link #CANONICAL_REQUEST_HEADER_JSON}. */
    static final byte[] CANONICAL_REQUEST_BODY = "{\"x\":1}".getBytes(StandardCharsets.UTF_8);

    @Test
    void crossLanguage_request_golden_bytes_are_locked() {
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("content-type", "application/json");

        byte[] wire = VesperaBridge.encodeRequest(
                "POST", "/users", "page=1", headers, CANONICAL_REQUEST_BODY);

        byte[] expectedHeader =
                CANONICAL_REQUEST_HEADER_JSON.getBytes(StandardCharsets.UTF_8);

        // Length prefix == exact canonical header byte length (big-endian).
        int headerLen = ByteBuffer.wrap(wire).order(ByteOrder.BIG_ENDIAN).getInt();
        assertEquals(expectedHeader.length, headerLen,
                "encoded header length drifted from the cross-language golden");

        // Header JSON bytes are byte-identical to the shared golden (locks the
        // Java encoder's field order + structure the Rust parser is asserted
        // to accept verbatim in wire_contract.rs).
        byte[] headerJson = new byte[headerLen];
        System.arraycopy(wire, 4, headerJson, 0, headerLen);
        assertArrayEquals(expectedHeader, headerJson,
                "request header JSON drifted from the cross-language golden — WIRE FORMAT BREAK");

        // Body follows the header verbatim.
        byte[] body = new byte[wire.length - 4 - headerLen];
        System.arraycopy(wire, 4 + headerLen, body, 0, body.length);
        assertArrayEquals(CANONICAL_REQUEST_BODY, body, "request body must follow header verbatim");
    }

    @Test
    void encodeRequestRejectsNullMethodAndPathWithFieldName() {
        NullPointerException method = assertThrows(
                NullPointerException.class,
                () -> VesperaBridge.encodeRequest(null, "/x", null, Map.of(), new byte[0]));
        NullPointerException path = assertThrows(
                NullPointerException.class,
                () -> VesperaBridge.encodeRequest("GET", null, null, Map.of(), new byte[0]));

        assertEquals("method", method.getMessage());
        assertEquals("path", path.getMessage());
    }

    @Test
    void encodeRequestRejectsNullHeaderKeyAndValueWithFieldName() {
        Map<String, String> nullKey = new HashMap<>();
        nullKey.put(null, "value");
        Map<String, String> nullValue = new HashMap<>();
        nullValue.put("x", null);

        NullPointerException key = assertThrows(
                NullPointerException.class,
                () -> VesperaBridge.encodeRequest("GET", "/x", null, nullKey, new byte[0]));
        NullPointerException value = assertThrows(
                NullPointerException.class,
                () -> VesperaBridge.encodeRequest("GET", "/x", null, nullValue, new byte[0]));

        assertEquals("header key", key.getMessage());
        assertEquals("header value", value.getMessage());
    }

    @Test
    void oversizedHeaderBufferShrinksWhenHeaderSourceThrows() {
        VesperaWireCodec.clearCurrentThreadBuffers();
        String huge = "x".repeat(40 * 1024);

        assertThrows(IllegalStateException.class, () -> VesperaBridge.encodeRequest(
                "GET",
                "/x",
                null,
                sink -> {
                    sink.put("x-big", huge);
                    throw new IllegalStateException("boom");
                },
                new byte[0]));

        assertEquals(256, VesperaWireCodec.currentHeaderBufferCapacityForTest());
    }

    @Test
    void directPoolShrinksOversizedHeaderBufferWhenDispatchThrows() {
        VesperaWireCodec.clearCurrentThreadBuffers();
        String huge = "x".repeat(40 * 1024);

        assertThrows(UnsatisfiedLinkError.class, () -> VesperaDirectBufferPool.dispatchDirectPooled(
                null,
                "GET",
                "/x",
                null,
                sink -> sink.put("x-big", huge),
                new byte[0],
                false,
                true));

        assertEquals(256, VesperaWireCodec.currentHeaderBufferCapacityForTest());
    }

    @Test
    void directPoolThrowPolicyRejectsHeapFallbackBeforeNativeDispatch() {
        String previous = System.getProperty("vespera.direct.oversize-policy");
        System.setProperty("vespera.direct.oversize-policy", "throw");
        try {
            VesperaBridge.BufferTooSmallException ex = assertThrows(
                    VesperaBridge.BufferTooSmallException.class,
                    () -> VesperaDirectBufferPool.dispatchDirectPooled(new byte[8], false, true));
            assertTrue(ex.getMessage().contains("vespera.direct.oversize-policy=throw"));
            assertEquals(8, ex.requiredSize());
        } finally {
            if (previous == null) {
                System.clearProperty("vespera.direct.oversize-policy");
            } else {
                System.setProperty("vespera.direct.oversize-policy", previous);
            }
        }
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
                new String(decoded.bodyBytes(), StandardCharsets.UTF_8));
        assertTrue(decoded.body().isReadOnly(), "body view must be read-only");
        assertEquals(0, decoded.body().position(), "body view position must start at 0");
        assertEquals("I'm a teapot".length(), decoded.body().limit(),
                "body view limit must equal body length");
    }

    @Test
    void decodedResponseNormalizesWritablePositionedBodyToReadOnlySlice() {
        ByteBuffer source = ByteBuffer.wrap(new byte[] {10, 20, 30});
        source.position(1);

        DecodedResponse decoded = new DecodedResponse(
                200, Map.of(), Map.of(), source, null);

        assertTrue(decoded.body().isReadOnly());
        assertEquals(0, decoded.body().position());
        assertArrayEquals(new byte[] {20, 30}, decoded.bodyBytes());
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
        assertArrayEquals(payload, decoded.bodyBytes(),
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
                decoded.bodyBytes(),
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

        assertArrayEquals(reqBody, decoded.bodyBytes());
    }

    /** Build a wire response whose headers map is supplied verbatim (so a
     * value may be a JSON array → multi-valued header). */
    private static byte[] buildWireResponseWithHeaders(
            int status, Map<String, Object> headers, byte[] body) throws Exception {
        Map<String, Object> headerMap = new LinkedHashMap<>();
        headerMap.put("v", 1);
        headerMap.put("status", status);
        headerMap.put("headers", headers);
        Map<String, Object> metadata = new LinkedHashMap<>();
        metadata.put("version", "0.1.51");
        headerMap.put("metadata", metadata);

        byte[] headerJson = MAPPER.writeValueAsBytes(headerMap);
        ByteBuffer buf = ByteBuffer.allocate(4 + headerJson.length + body.length)
                .order(ByteOrder.BIG_ENDIAN);
        buf.putInt(headerJson.length);
        buf.put(headerJson);
        buf.put(body);
        return buf.array();
    }

    @Test
    void decodeResponse_parses_multi_value_header_as_list() throws Exception {
        // Repeated header names (e.g. set-cookie) arrive as a JSON array on
        // the wire and must decode to a List<String>, not a String.
        Map<String, Object> headers = new LinkedHashMap<>();
        headers.put("content-type", "text/plain");
        headers.put("set-cookie", List.of("a=1; Path=/", "b=2; HttpOnly"));

        byte[] wire = buildWireResponseWithHeaders(
                200, headers, "ok".getBytes(StandardCharsets.UTF_8));
        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);

        assertEquals(200, decoded.status());
        assertEquals("text/plain", decoded.headers().get("content-type"));
        Object setCookie = decoded.headers().get("set-cookie");
        assertTrue(setCookie instanceof List, "multi-valued header must decode to a List");
        assertEquals(List.of("a=1; Path=/", "b=2; HttpOnly"), setCookie);
    }

    @Test
    void decodeResponse_publicCollectionsAreImmutableCopies() throws Exception {
        Map<String, Object> headers = new LinkedHashMap<>();
        headers.put("set-cookie", List.of("a=1", "b=2"));
        List<Map<String, Object>> errors = new ArrayList<>();
        Map<String, Object> error = new LinkedHashMap<>();
        error.put("path", "name");
        errors.add(error);

        DecodedResponse decoded = VesperaBridge.decodeResponse(buildWireResponseWithExtras(
                422, "application/json", new byte[0], errors));
        DecodedResponse multiHeader = VesperaBridge.decodeResponse(
                buildWireResponseWithHeaders(200, headers, new byte[0]));

        assertThrows(UnsupportedOperationException.class,
                () -> decoded.metadata().put("x", "y"));
        assertThrows(UnsupportedOperationException.class,
                () -> decoded.validationErrors().add(Map.of()));
        assertThrows(UnsupportedOperationException.class,
                () -> decoded.validationErrors().get(0).put("message", "changed"));
        assertThrows(UnsupportedOperationException.class,
                () -> multiHeader.headers().put("x", "y"));
        @SuppressWarnings("unchecked")
        List<String> setCookie = (List<String>) multiHeader.headers().get("set-cookie");
        assertThrows(UnsupportedOperationException.class,
                () -> setCookie.add("c=3"));
    }

    @Test
    void decodeResponse_handles_escaped_and_non_ascii_header_values() throws Exception {
        // The header value carries a JSON-escaped quote and multi-byte UTF-8,
        // exercising the reader's escape + UTF-8 decode path (not the plain
        // ASCII fast path).
        Map<String, Object> headers = new LinkedHashMap<>();
        headers.put("x-note", "say \"hi\" 한글");

        byte[] wire = buildWireResponseWithHeaders(200, headers, new byte[0]);
        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);

        assertEquals("say \"hi\" 한글", decoded.headers().get("x-note"));
    }

    @Test
    void encodeRequest_escapes_special_and_unicode_in_values() throws Exception {
        // Lock the byte-direct encoder's escaping: quote, backslash, tab and
        // newline (C0 short escapes), 3-byte UTF-8 (한글), and a 4-byte
        // supplementary char via surrogate pair (😀, U+1F600) — in path,
        // query, and header values.  The produced bytes must be valid JSON
        // that parses back to the exact originals (the contract the Rust
        // serde_json side relies on).
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("x-quote", "a\"b\\c\td\ne");
        headers.put("x-unicode", "한글-😀");

        byte[] wire = VesperaBridge.encodeRequest(
                "POST", "/p\"a\\th/한글", "q=\"x\"&한=글", headers, new byte[0]);

        int headerLen = ByteBuffer.wrap(wire).order(ByteOrder.BIG_ENDIAN).getInt();
        byte[] headerJson = new byte[headerLen];
        System.arraycopy(wire, 4, headerJson, 0, headerLen);
        JsonNode h = MAPPER.readTree(headerJson);

        assertEquals("POST", h.path("method").asText());
        // path and query are each JSON-escaped, then joined by a literal '?'
        // into the single `path` request target — no separate `query` field.
        // Independently re-parsed by Jackson, so a mis-escape here fails loudly.
        assertEquals("/p\"a\\th/한글?q=\"x\"&한=글", h.path("path").asText());
        assertEquals("", h.path("query").asText());
        assertEquals("a\"b\\c\td\ne", h.path("headers").path("x-quote").asText());
        assertEquals("한글-😀", h.path("headers").path("x-unicode").asText());
    }

    @Test
    void decodeResponse_canonical_and_custom_header_keys_both_parse() throws Exception {
        // content-type is a canonical (interned, allocation-free) key;
        // x-custom-trace is not and must still parse via the readString
        // fallback — both values, and the canonical metadata "version" key,
        // round-trip exactly.  Guards the peek/consume cursor bookkeeping.
        Map<String, Object> headers = new LinkedHashMap<>();
        headers.put("content-type", "application/json");
        headers.put("x-custom-trace", "abc-123");

        byte[] wire = buildWireResponseWithHeaders(
                200, headers, "ok".getBytes(StandardCharsets.UTF_8));
        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);

        assertEquals("application/json", decoded.headers().get("content-type"));
        assertEquals("abc-123", decoded.headers().get("x-custom-trace"));
        assertEquals("0.1.51", decoded.metadata().get("version"));
    }

    @Test
    void decodeResponse_multi_entry_metadata_parses_all_keys() throws Exception {
        // Metadata with 2 keys (the rare path): canonical "version" plus a
        // custom "build" key.  Both must round-trip — exercises the
        // LinkedHashMap fallback in readStringMap (single-entry uses Map.of).
        Map<String, Object> headerMap = new LinkedHashMap<>();
        headerMap.put("v", 1);
        headerMap.put("status", 200);
        headerMap.put("headers", new LinkedHashMap<>());
        Map<String, Object> metadata = new LinkedHashMap<>();
        metadata.put("version", "0.1.51");
        metadata.put("build", "deadbeef");
        headerMap.put("metadata", metadata);

        byte[] headerJson = MAPPER.writeValueAsBytes(headerMap);
        ByteBuffer buf = ByteBuffer.allocate(4 + headerJson.length).order(ByteOrder.BIG_ENDIAN);
        buf.putInt(headerJson.length);
        buf.put(headerJson);

        DecodedResponse decoded = VesperaBridge.decodeResponse(buf.array());
        assertEquals(2, decoded.metadata().size());
        assertEquals("0.1.51", decoded.metadata().get("version"));
        assertEquals("deadbeef", decoded.metadata().get("build"));
    }

    @Test
    void decodeResponse_empty_headers_yields_empty_map() throws Exception {
        // Headers object present but empty -> readHeaderMap returns null ->
        // decodeResponse substitutes the shared empty map.  (Single-header
        // responses take the Map.of path, covered by the status/headers/body
        // test; 2+ headers take the LinkedHashMap path, covered by the
        // multi-value header test.)
        byte[] wire = buildWireResponseWithHeaders(
                200, new LinkedHashMap<>(), "ok".getBytes(StandardCharsets.UTF_8));
        DecodedResponse decoded = VesperaBridge.decodeResponse(wire);

        assertEquals(200, decoded.status());
        assertTrue(decoded.headers().isEmpty(), "empty headers object yields an empty map");
    }

    @Test
    void nullHeadersNullBodyBlankAppAndMapFailurePathsAreSpecified() throws Exception {
        byte[] wire = VesperaWireCodec.encodeRequest("   ", "GET", "/x", null,
                (Map<String, String>) null, null);
        int headerLen = ByteBuffer.wrap(wire).getInt();
        JsonNode header = MAPPER.readTree(wire, 4, headerLen);
        assertTrue(header.path("headers").isMissingNode());
        assertTrue(header.path("app").isMissingNode());
        assertEquals(4 + headerLen, wire.length);

        Map<String, String> invalid = new HashMap<>();
        invalid.put(null, "value");
        NullPointerException error = assertThrows(
                NullPointerException.class,
                () -> VesperaWireCodec.encodeRequest(null, "GET", "/x", null, invalid, null));
        assertEquals("header key", error.getMessage());
    }

    @Test
    void headerSourceConvenienceOverloadsEncodeDefaultAndNamedApps() throws Exception {
        VesperaBridge.HeaderSource headers = sink -> sink.put("x-test", "yes");

        byte[] defaultHeader = VesperaBridge.encodeRequestHeader(
                "GET", "/default", null, headers);
        byte[] namedHeader = VesperaBridge.encodeRequestHeader(
                "admin", "POST", "/named", "a=1", headers);
        byte[] request = VesperaBridge.encodeRequest(
                "PUT", "/request", null, headers, new byte[] {7, 8});

        JsonNode defaultJson = headerJson(defaultHeader);
        JsonNode namedJson = headerJson(namedHeader);
        JsonNode requestJson = headerJson(request);
        assertTrue(defaultJson.path("app").isMissingNode());
        assertEquals("yes", defaultJson.path("headers").path("x-test").asText());
        assertEquals("admin", namedJson.path("app").asText());
        assertEquals("/named?a=1", namedJson.path("path").asText());
        assertEquals("PUT", requestJson.path("method").asText());
        assertArrayEquals(new byte[] {7, 8}, java.util.Arrays.copyOfRange(
                request, 4 + ByteBuffer.wrap(request).getInt(), request.length));
    }

    @Test
    void headerSourceOverloadRejectsQuestionMarkInPathWithExactMessage() {
        IllegalArgumentException error = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaBridge.encodeRequest(
                        "GET", "/items?page=1", null,
                        (VesperaBridge.HeaderSource) sink -> {}, new byte[0]));

        assertEquals(
                "path must not contain '?' — pass the raw query string via the query parameter",
                error.getMessage());
    }

    private static JsonNode headerJson(byte[] wire) throws Exception {
        int headerLen = ByteBuffer.wrap(wire).getInt();
        return MAPPER.readTree(wire, 4, headerLen);
    }

    @Test
    void byteBufferHeaderLengthFailuresArePrecise() {
        IllegalArgumentException shortError = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaWireCodec.readHeaderLength(ByteBuffer.allocateDirect(3)));
        assertEquals("wire response too short: 3 bytes", shortError.getMessage());

        ByteBuffer overflow = ByteBuffer.allocateDirect(4);
        overflow.putInt(32);
        IllegalArgumentException overflowError = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaWireCodec.readHeaderLength(overflow));
        assertEquals("wire header_len 32 overflows response (4 bytes)", overflowError.getMessage());

        IllegalArgumentException nullError = assertThrows(
                IllegalArgumentException.class,
                () -> VesperaWireCodec.readHeaderLength((byte[]) null));
        assertEquals("wire response too short: null", nullError.getMessage());
    }

    @Test
    void exposedHeaderBufferGrowthAndBoundsAreSpecified() throws Exception {
        VesperaWireCodec.ExposedByteArrayOutputStream bytes =
                new VesperaWireCodec.ExposedByteArrayOutputStream(0);
        bytes.put('a');
        bytes.putAscii("bc");
        bytes.putAsciiSlice("ignored", 3, 3);

        assertEquals(4, bytes.capacity());
        assertEquals("abc", new String(bytes.backingArray(), 0, bytes.size(), StandardCharsets.US_ASCII));

        Method growCap = VesperaWireCodec.ExposedByteArrayOutputStream.class
                .getDeclaredMethod("growCap", int.class, int.class);
        growCap.setAccessible(true);
        assertEquals(64 * 1024 * 1024, growCap.invoke(null, 40 * 1024 * 1024, 60 * 1024 * 1024));
        InvocationTargetException wrapped = assertThrows(
                InvocationTargetException.class,
                () -> growCap.invoke(null, 1, 64 * 1024 * 1024 + 1));
        IllegalArgumentException tooLarge = assertInstanceOf(
                IllegalArgumentException.class, wrapped.getCause());
        assertEquals("wire header exceeds 67108864 bytes", tooLarge.getMessage());
    }

    @Test
    void directAsciiScratchGrowsThenUsesLargeTemporaryArray() {
        WireHeaderReader.clearCurrentThreadBuffers();
        String medium = "m".repeat(300);
        ByteBuffer mediumBuffer = ByteBuffer.allocateDirect(medium.length());
        mediumBuffer.put(medium.getBytes(StandardCharsets.US_ASCII));
        String large = "l".repeat(9 * 1024);
        ByteBuffer largeBuffer = ByteBuffer.allocateDirect(large.length());
        largeBuffer.put(large.getBytes(StandardCharsets.US_ASCII));

        assertEquals(medium, WireHeaderStringSupport.readAsciiString(mediumBuffer, 0, medium.length()));
        assertEquals(large, WireHeaderStringSupport.readAsciiString(largeBuffer, 0, large.length()));
    }

    @Test
    void oversizedReusableBufferIsLazilyReplaced() {
        VesperaWireCodec.clearCurrentThreadBuffers();
        VesperaWireCodec.ExposedByteArrayOutputStream oversized = VesperaWireCodec.fillHeaderJson(
                null, "GET", "/x", null, Map.of("x-big", "x".repeat(40 * 1024)));
        assertTrue(oversized.capacity() > 32 * 1024);

        VesperaWireCodec.ExposedByteArrayOutputStream replacement =
                VesperaWireCodec.fillHeaderJson(null, "GET", "/next", null, Map.of());

        assertEquals(256, replacement.capacity());
        assertTrue(new String(replacement.backingArray(), 0, replacement.size(), StandardCharsets.UTF_8)
                .contains("/next"));
    }
}
