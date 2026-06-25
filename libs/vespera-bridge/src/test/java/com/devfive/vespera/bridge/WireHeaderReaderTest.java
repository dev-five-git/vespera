package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;

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
        return runWith(hb, direct);
    }

    private static Captured runWith(byte[] hb, boolean direct) {
        ByteBuffer buf =
                direct ? ByteBuffer.allocateDirect(4 + hb.length) : ByteBuffer.allocate(4 + hb.length);
        buf.putInt(hb.length);
        buf.put(hb);
        List<String> headers = new ArrayList<>();
        int status = WireHeaderReader.apply(
                buf, 4, hb.length, (k, v) -> headers.add(k + "=" + v));
        return new Captured(status, headers);
    }

    private static void assertRejected(byte[] headerBytes) {
        assertThrows(IllegalArgumentException.class, () -> runWith(headerBytes, true));
        assertThrows(IllegalArgumentException.class, () -> runWith(headerBytes, false));
    }

    private static void assertDecodeRejected(String headerJson) {
        byte[] hb = headerJson.getBytes(StandardCharsets.UTF_8);
        ByteBuffer buf = ByteBuffer.allocate(4 + hb.length);
        buf.putInt(hb.length);
        buf.put(hb);
        IllegalArgumentException e = assertThrows(
                IllegalArgumentException.class,
                () -> WireHeaderReader.decode(buf, 4, hb.length));
        assertEquals("wire header JSON: expected object at offset 4", e.getMessage());
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
                        "{\"status\":200,\"headers\":{\"x-q\":\"a\\\"b\\\\c\\n\",\"x-u\":\"caf\u00e9\"," 
                                + "\"x-emoji\":\"\uD83D\uDE80\"}}");
        assertEquals(200, c.status());
        assertEquals(List.of("x-q=a\"b\\c\n", "x-u=caf\u00e9", "x-emoji=\uD83D\uDE80"), c.headers());
    }

    @Test
    void handlesEscapedUnicodeSurrogatePairInValues() {
        Captured c = run("{\"status\":200,\"headers\":{\"x-emoji\":\"\\uD83D\\uDE00\"}}");

        assertEquals(200, c.status());
        assertEquals(List.of("x-emoji=\uD83D\uDE00"), c.headers());
    }

    @Test
    void rejectsLoneEscapedUnicodeSurrogates() {
        assertRejected("{\"status\":200,\"headers\":{\"x\":\"\\uD800\"}}".getBytes(StandardCharsets.UTF_8));
        assertRejected("{\"status\":200,\"headers\":{\"x\":\"\\uDC00\"}}".getBytes(StandardCharsets.UTF_8));
        assertRejected("{\"status\":200,\"headers\":{\"x\":\"\\uD800\\u0041\"}}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void rejectsStatusIntegerOverflow() {
        assertRejected("{\"status\":2147483648}".getBytes(StandardCharsets.UTF_8));
        assertRejected("{\"status\":-2147483649}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void rejectsDecimalOrExponentStatus() {
        // `status` is a protocol INTEGER field; `200.9` / `2e2` are malformed
        // native output and must be REJECTED, not silently truncated to the
        // integer part.  Unknown numeric fields stay permissive — see
        // skipsUnknownLargeAndDecimalNumericFields.
        assertRejected("{\"status\":200.9}".getBytes(StandardCharsets.UTF_8));
        assertRejected("{\"status\":2e2}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void rejectsTrailingGarbageAfterRootObject() {
        byte[] headerBytes = "{\"status\":200}junk".getBytes(StandardCharsets.UTF_8);
        assertRejected(headerBytes);

        ByteBuffer buf = ByteBuffer.allocate(4 + headerBytes.length);
        buf.putInt(headerBytes.length);
        buf.put(headerBytes);
        assertThrows(IllegalArgumentException.class, () -> WireHeaderReader.decode(buf, 4, headerBytes.length));
    }

    @Test
    void rejectsStatusOutsideWireHttpRange() {
        assertRejected("{\"status\":99}".getBytes(StandardCharsets.UTF_8));
        assertRejected("{\"status\":1000}".getBytes(StandardCharsets.UTF_8));
        assertRejected("{\"status\":-200}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void rejectsMalformedUtf8ContinuationAndOverlongSequences() {
        assertRejected(new byte[] {
            '{', '"', 's', 't', 'a', 't', 'u', 's', '"', ':', '2', '0', '0', ',',
            '"', 'h', 'e', 'a', 'd', 'e', 'r', 's', '"', ':', '{', '"', 'x', '"', ':', '"',
            (byte) 0xC3, '(', '"', '}', '}'
        });
        assertRejected(new byte[] {
            '{', '"', 's', 't', 'a', 't', 'u', 's', '"', ':', '2', '0', '0', ',',
            '"', 'h', 'e', 'a', 'd', 'e', 'r', 's', '"', ':', '{', '"', 'x', '"', ':', '"',
            (byte) 0xC0, (byte) 0x80, '"', '}', '}'
        });
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

    @Test
    void rejectsNonObjectRootHeader() {
        byte[] headerBytes = "[]".getBytes(StandardCharsets.UTF_8);
        assertRejected(headerBytes);
        assertDecodeRejected("[]");
    }

    @Test
    void rejectsDuplicateStatusRootKey() {
        assertRejected("{\"status\":200,\"status\":201}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void rejectsDuplicateHeadersRootKey() {
        assertRejected(
                ("{\"status\":200,\"headers\":{\"a\":\"b\"},"
                        + "\"headers\":{\"c\":\"d\"}}").getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void rejectsMalformedSkippedLiteral() {
        assertRejected("{\"status\":200,\"unknown\":truth}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void skipsUnknownLargeAndDecimalNumericFields() {
        // Forward-compat: an UNKNOWN numeric field beyond int range, or a
        // decimal / exponent, must be skipped as a raw token — NOT parsed
        // and overflow-rejected like the known `status` field (which the
        // prior readInt-based skip did, failing decode of an otherwise-valid
        // header).  The known `status` is still parsed normally.
        Captured c =
                run(
                        "{\"status\":200,\"ts\":1700000000000000000,\"ratio\":-3.14e2,"
                                + "\"headers\":{\"content-type\":\"text/plain\"}}");
        assertEquals(200, c.status());
        assertEquals(List.of("content-type=text/plain"), c.headers());
    }

    /**
     * P3: {@code apply()} now routes common header names through the shared
     * canonical-key matcher (the same allocation-free path {@code
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
        WireHeaderReader.apply(buf, 4, hb.length, (k, v) -> applyKey[0] = k);

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
