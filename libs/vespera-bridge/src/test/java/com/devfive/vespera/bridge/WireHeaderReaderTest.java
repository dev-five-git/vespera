package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.lang.reflect.Constructor;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
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

    private static WireHeaderReader.Decoded decode(String headerJson) {
        byte[] bytes = headerJson.getBytes(StandardCharsets.UTF_8);
        WireHeaderReader.Decoded heap = WireHeaderReader.decode(bytes, 0, bytes.length);
        ByteBuffer direct = ByteBuffer.allocateDirect(bytes.length);
        direct.put(bytes);
        WireHeaderReader.Decoded directDecoded = WireHeaderReader.decode(direct, 0, bytes.length);
        assertEquals(heap.status, directDecoded.status);
        assertEquals(heap.headers, directDecoded.headers);
        assertEquals(heap.metadata, directDecoded.metadata);
        assertEquals(heap.validationErrors, directDecoded.validationErrors);
        return heap;
    }

    private static WireHeaderReader reflectedReader(String json) throws Exception {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        Constructor<WireHeaderReader> constructor =
                WireHeaderReader.class.getDeclaredConstructor(byte[].class, int.class, int.class);
        constructor.setAccessible(true);
        return constructor.newInstance(bytes, 0, bytes.length);
    }

    private static IllegalArgumentException invokeRejected(
            WireHeaderReader reader, String methodName) throws Exception {
        Method method = WireHeaderReader.class.getDeclaredMethod(methodName);
        method.setAccessible(true);
        InvocationTargetException wrapped = assertThrows(
                InvocationTargetException.class, () -> method.invoke(reader));
        return assertInstanceOf(IllegalArgumentException.class, wrapped.getCause());
    }

    private static void assertDecodeFailure(String json, String expectedMessage) {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        IllegalArgumentException error = assertThrows(
                IllegalArgumentException.class,
                () -> WireHeaderReader.decode(bytes, 0, bytes.length));
        assertEquals(expectedMessage, error.getMessage());
    }

    private static void assertDecodeFailure(byte[] json, String expectedMessage) {
        IllegalArgumentException error = assertThrows(
                IllegalArgumentException.class,
                () -> WireHeaderReader.decode(json, 0, json.length));
        assertEquals(expectedMessage, error.getMessage());
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

    @Test
    void decodesEveryJsonEscapeAndWhitespaceForm() {
        WireHeaderReader.Decoded decoded = decode(
                " \t\n\r{\"status\" : 200, \"headers\":{\"x\":"
                        + "\"\\\"\\\\\\/\\b\\f\\n\\r\\t\\u00e9\"},\"metadata\":{}} \r\n");

        assertEquals("\"\\/\b\f\n\r\té", decoded.headers.get("x"));
        assertEquals(Map.of(), decoded.metadata);
    }

    @Test
    void decodesForwardCompatibleValidationPrimitiveTypes() {
        WireHeaderReader.Decoded decoded = decode(
                "{\"status\":422,\"headers\":null,\"metadata\":null,"
                        + "\"validation_errors\":[7,{\"truth\":true,\"lie\":false,"
                        + "\"nil\":null,\"integer\":-12,\"decimal\":1.5e+2,"
                        + "\"huge\":9223372036854775808,\"nested\":{\"ignored\":1}}]}");

        assertNull(decoded.headers);
        assertEquals(Map.of(), decoded.metadata);
        assertEquals(1, decoded.validationErrors.size());
        Map<String, Object> error = decoded.validationErrors.get(0);
        assertEquals(Boolean.TRUE, error.get("truth"));
        assertEquals(Boolean.FALSE, error.get("lie"));
        assertNull(error.get("nil"));
        assertEquals(-12L, error.get("integer"));
        assertEquals(150.0, error.get("decimal"));
        assertInstanceOf(Double.class, error.get("huge"));
        assertNull(error.get("nested"));
    }

    @Test
    void skipsWronglyShapedValidationFieldAndReadsThreeMetadataEntries() {
        WireHeaderReader.Decoded decoded = decode(
                "{\"status\":200,\"validation_errors\":null,\"metadata\":{"
                        + "\"version\":\"1\",\"build\":\"2\",\"date\":\"3\"}}");

        assertNull(decoded.validationErrors);
        assertEquals(Map.of("version", "1", "build", "2", "date", "3"), decoded.metadata);
    }

    @Test
    void skipsUnknownStringsLiteralsAndEscapedNestedContainers() {
        Captured captured = run(
                "{\"unknownString\":\"a\\\"b\",\"yes\":true,\"no\":false,"
                        + "\"nested\":{\"text\":\"}\\\"]\"},\"status\":204}");

        assertEquals(204, captured.status());
        assertEquals(List.of(), captured.headers());
    }

    @Test
    void malformedTokensReportPreciseParserFailure() {
        Map<String, String> malformed = Map.ofEntries(
                Map.entry("{status:200}", "expected string"),
                Map.entry("{\"status\" 200}", "expected ':'"),
                Map.entry("{\"status\":}", "expected number"),
                Map.entry("{\"headers\":{\"x\":1}}", "expected string"),
                Map.entry("{\"headers\":{\"x\":\"\\", "dangling escape"),
                Map.entry("{\"headers\":{\"x\":\"\\q\"}}", "bad escape"),
                Map.entry("{\"headers\":{\"x\":\"\\u12\"}}", "bad hex digit"),
                Map.entry("{\"headers\":{\"x\":\"\\u12xz\"}}", "bad hex digit"),
                Map.entry("{\"unknown\":?}", "unexpected value"),
                Map.entry("{\"unknown\":-}", "expected number"),
                Map.entry("{\"unknown\":\"unterminated}", "unterminated string"),
                Map.entry("{\"unknown\":[1", "unterminated container"));

        malformed.forEach((json, expected) -> {
            IllegalArgumentException error = assertThrows(
                    IllegalArgumentException.class,
                    () -> WireHeaderReader.decode(json.getBytes(StandardCharsets.UTF_8), 0,
                            json.getBytes(StandardCharsets.UTF_8).length));
            org.junit.jupiter.api.Assertions.assertTrue(error.getMessage().contains(expected), error.getMessage());
        });
    }

    @Test
    void malformedUtf8CoversTruncationOverlongSurrogateAndOutOfRangeForms() {
        byte[][] invalidValues = {
            {(byte) 0xE0, (byte) 0x80, (byte) 0x80},
            {(byte) 0xED, (byte) 0xA0, (byte) 0x80},
            {(byte) 0xF0, (byte) 0x80, (byte) 0x80, (byte) 0x80},
            {(byte) 0xF4, (byte) 0x90, (byte) 0x80, (byte) 0x80},
            {(byte) 0xF5, (byte) 0x80, (byte) 0x80, (byte) 0x80},
            {(byte) 0xE2}
        };
        byte[] prefix = "{\"headers\":{\"x\":\"".getBytes(StandardCharsets.UTF_8);
        byte[] suffix = "\"}}".getBytes(StandardCharsets.UTF_8);

        for (byte[] invalid : invalidValues) {
            byte[] json = new byte[prefix.length + invalid.length + suffix.length];
            System.arraycopy(prefix, 0, json, 0, prefix.length);
            System.arraycopy(invalid, 0, json, prefix.length, invalid.length);
            System.arraycopy(suffix, 0, json, prefix.length + invalid.length, suffix.length);
            IllegalArgumentException error = assertThrows(
                    IllegalArgumentException.class,
                    () -> WireHeaderReader.decode(json, 0, json.length));
            org.junit.jupiter.api.Assertions.assertTrue(
                    error.getMessage().contains("UTF-8"), error.getMessage());
        }
    }

    @Test
    void legacyNextKeyAndOtherwiseUnreachableLiteralGuardRemainSpecified() throws Exception {
        WireHeaderReader reader = reflectedReader("{\"a\":1,\"b\":2}");
        reader.beginObject();

        assertEquals("a", reader.nextKey());
        reader.skipValue();
        assertEquals("b", reader.nextKey());
        reader.skipValue();
        assertNull(reader.nextKey());

        IllegalArgumentException error = invokeRejected(reflectedReader("x"), "skipLiteral");
        assertEquals("wire header JSON: expected literal at offset 0", error.getMessage());
    }

    @Test
    void currentByteReturnsDataThenEndSentinel() throws Exception {
        WireHeaderReader reader = reflectedReader("7");
        Method cur = WireHeaderReader.class.getDeclaredMethod("cur");
        cur.setAccessible(true);
        assertEquals((int) '7', cur.invoke(reader));
        reader.skipValue();
        assertEquals(-1, cur.invoke(reader));
    }

    @Test
    void canonicalKeyFallbackParsesEscapedAndUtf8Keys() {
        WireHeaderReader.Decoded decoded = decode(
                "{\"status\":200,\"headers\":{\"content\\u002dtype\":\"text/plain\","
                        + "\"café\":\"yes\"},\"metadata\":{\"ver\\u0073ion\":\"1\"}}");

        assertEquals("text/plain", decoded.headers.get("content-type"));
        assertEquals("yes", decoded.headers.get("café"));
        assertEquals("1", decoded.metadata.get("version"));
    }

    @Test
    void canonicalKeyProbeRejectsNonStringAndUnterminatedKeysPrecisely() throws Exception {
        IllegalArgumentException nonString = invokeRejected(
                reflectedReader("7"), "nextKeyCanonical");
        assertEquals("wire header JSON: expected string at offset 0", nonString.getMessage());

        String unterminated = "\"unterminated";
        IllegalArgumentException missingQuote = invokeRejected(
                reflectedReader(unterminated), "nextKeyCanonical");
        assertEquals(
                "wire header JSON: unterminated string at offset " + unterminated.length(),
                missingQuote.getMessage());
    }

    @Test
    void rootKeyMatcherSkipsEscapedKeysAndRejectsUnterminatedKeysPrecisely() {
        WireHeaderReader.Decoded decoded = decode("{\"sta\\\"tus\":200}");
        assertEquals(500, decoded.status);

        String unterminated = "{\"status";
        assertDecodeFailure(
                unterminated,
                "wire header JSON: unterminated string at offset " + unterminated.length());
    }

    @Test
    void validationPrimitiveNumberErrorsReportExactOffsets() {
        String unexpected = "{\"validation_errors\":[{\"x\":?}]}";
        assertDecodeFailure(
                unexpected,
                "wire header JSON: unexpected primitive value at offset " + unexpected.indexOf('?'));

        String decimal = "{\"validation_errors\":[{\"x\":1.}]}";
        assertDecodeFailure(
                decimal,
                "wire header JSON: expected digit after decimal point at offset "
                        + (decimal.indexOf("1.") + 2));

        String exponent = "{\"validation_errors\":[{\"x\":1e}]}";
        assertDecodeFailure(
                exponent,
                "wire header JSON: expected digit in exponent at offset "
                        + (exponent.indexOf("1e") + 2));

        String signOnly = "{\"validation_errors\":[{\"x\":-}]}";
        assertDecodeFailure(
                signOnly,
                "wire header JSON: expected number at offset " + signOnly.indexOf('-'));
    }

    @Test
    void truncatedUtf8AndUnicodeEscapeReportExactOffsets() {
        byte[] utf8Prefix = "{\"headers\":{\"x\":\"".getBytes(StandardCharsets.UTF_8);
        byte[] truncatedUtf8 = java.util.Arrays.copyOf(utf8Prefix, utf8Prefix.length + 1);
        truncatedUtf8[utf8Prefix.length] = (byte) 0xE2;
        assertDecodeFailure(
                truncatedUtf8,
                "wire header JSON: truncated UTF-8 at offset " + truncatedUtf8.length);

        String truncatedUnicode = "{\"headers\":{\"x\":\"\\u12";
        assertDecodeFailure(
                truncatedUnicode,
                "wire header JSON: truncated unicode escape at offset "
                        + (truncatedUnicode.length() - 2));
    }
}
