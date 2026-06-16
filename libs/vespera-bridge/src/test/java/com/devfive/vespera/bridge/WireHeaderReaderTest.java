package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertSame;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

/** Correctness gate for the zero-copy DIRECT-path header reader. */
class WireHeaderReaderTest {

    private record Captured(int status, List<String> headers) {}

    /**
     * Parse {@code headerJson} through BOTH a direct buffer (the DIRECT
     * dispatch path, no backing array) and a heap buffer (the SYNC /
     * streaming / async {@code ByteBuffer.wrap} paths, which hit
     * {@code readString}'s backing-array fast path), asserting the two
     * agree.  Returns the (identical) result.
     */
    private static Captured run(String headerJson) {
        Captured direct = runWith(headerJson, true);
        Captured heap = runWith(headerJson, false);
        assertEquals(direct.status(), heap.status(), "direct vs heap status mismatch");
        assertEquals(direct.headers(), heap.headers(), "direct vs heap headers mismatch");
        return direct;
    }

    private static Captured runWith(String headerJson, boolean direct) {
        byte[] hb = headerJson.getBytes(StandardCharsets.UTF_8);
        ByteBuffer buf =
                direct ? ByteBuffer.allocateDirect(4 + hb.length) : ByteBuffer.allocate(4 + hb.length);
        buf.putInt(hb.length);
        buf.put(hb);
        int[] status = {-1};
        List<String> headers = new ArrayList<>();
        WireHeaderReader.apply(
                buf, 4, hb.length, s -> status[0] = s, (k, v) -> headers.add(k + "=" + v));
        return new Captured(status[0], headers);
    }

    @Test
    void parsesStatusAndSingleHeader() {
        Captured c =
                run(
                        "{\"v\":1,\"status\":200,\"headers\":{\"content-type\":\"text/plain\"},"
                                + "\"metadata\":{\"version\":\"0.1.0\"}}");
        assertEquals(200, c.status());
        assertEquals(List.of("content-type=text/plain"), c.headers());
    }

    @Test
    void parsesMultiValuedHeaderArray() {
        Captured c =
                run(
                        "{\"v\":1,\"status\":201,\"headers\":{\"set-cookie\":[\"a=1\",\"b=2\"],"
                                + "\"x\":\"y\"}}");
        assertEquals(201, c.status());
        assertEquals(List.of("set-cookie=a=1", "set-cookie=b=2", "x=y"), c.headers());
    }

    @Test
    void handlesEscapesAndUtf8InValues() {
        Captured c =
                run(
                        "{\"status\":200,\"headers\":{\"x-q\":\"a\\\"b\\\\c\\n\",\"x-u\":\"caf\u00e9\"}}");
        assertEquals(200, c.status());
        assertEquals(List.of("x-q=a\"b\\c\n", "x-u=caf\u00e9"), c.headers());
    }

    @Test
    void statusAbsentDefaultsTo500() {
        Captured c = run("{\"v\":1,\"headers\":{\"a\":\"b\"}}");
        assertEquals(500, c.status());
        assertEquals(List.of("a=b"), c.headers());
    }

    @Test
    void emptyHeadersAndEmptyMetadataDoNotCorruptParsing() {
        // The exact shape (empty nested object before another field) that broke
        // a prior stateful reader.
        Captured c = run("{\"v\":1,\"status\":204,\"headers\":{},\"metadata\":{}}");
        assertEquals(204, c.status());
        assertEquals(List.of(), c.headers());
    }

    @Test
    void skipsUnknownNestedAndArrayFields() {
        Captured c =
                run(
                        "{\"status\":422,\"validation_errors\":[{\"path\":\"a\",\"message\":\"m\"}],"
                                + "\"headers\":{\"content-type\":\"application/json\"}}");
        assertEquals(422, c.status());
        assertEquals(List.of("content-type=application/json"), c.headers());
    }

    @Test
    void nonObjectHeaderIsSkipped() {
        Captured c = run("{\"status\":200,\"headers\":null}");
        assertEquals(200, c.status());
        assertEquals(List.of(), c.headers());
    }

    /**
     * P3: {@code apply()} now routes common header names through the shared
     * {@code CANONICAL_KEYS} table (the same allocation-free path {@code
     * decode()} uses), so the key String it hands back is the interned
     * instance — not a freshly allocated one per request. Asserting identity
     * ({@code assertSame}) against {@code decode()}'s key locks that in.
     */
    @Test
    void applyReusesCanonicalKeyInstances() {
        String json = "{\"status\":200,\"headers\":{\"content-type\":\"x\"}}";
        byte[] hb = json.getBytes(StandardCharsets.UTF_8);

        ByteBuffer buf = ByteBuffer.allocate(4 + hb.length);
        buf.putInt(hb.length);
        buf.put(hb);
        String[] applyKey = {null};
        WireHeaderReader.apply(buf, 4, hb.length, s -> {}, (k, v) -> applyKey[0] = k);

        ByteBuffer buf2 = ByteBuffer.allocate(4 + hb.length);
        buf2.putInt(hb.length);
        buf2.put(hb);
        WireHeaderReader.Decoded decoded = WireHeaderReader.decode(buf2, 4, hb.length);
        String decodeKey = decoded.headers.keySet().iterator().next();

        assertSame(
                decodeKey,
                applyKey[0],
                "apply() must hand back the same canonical key instance decode() uses");
    }
}
