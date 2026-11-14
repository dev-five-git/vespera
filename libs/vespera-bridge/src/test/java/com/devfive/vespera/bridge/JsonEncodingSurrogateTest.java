package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.ByteBuffer;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import org.junit.jupiter.api.Test;

/**
 * The manual JSON encoder (exercised through {@link VesperaBridge#encodeRequest}
 * and {@link VesperaWireCodec#writePrincipal}) must encode <em>unpaired</em>
 * UTF-16 surrogates as U+FFFD (the replacement character, like
 * {@code String.getBytes(UTF_8)}): neither an invalid 3-byte UTF-8 sequence
 * (not valid UTF-8) nor a {@code \\uD800} escape (valid JSON, but it names no
 * Unicode scalar value, so the Rust wire parser answers {@code 400}).
 */
class JsonEncodingSurrogateTest {

    private static final String REPLACEMENT = "�";

    /** Extract the JSON header region and assert it is strictly valid UTF-8. */
    private static String headerJson(byte[] wire) {
        int len = ((wire[0] & 0xFF) << 24)
                | ((wire[1] & 0xFF) << 16)
                | ((wire[2] & 0xFF) << 8)
                | (wire[3] & 0xFF);
        return strictUtf8(wire, 4, len);
    }

    private static String strictUtf8(byte[] bytes, int off, int len) {
        var decoder = StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        assertDoesNotThrow(
                () -> decoder.decode(ByteBuffer.wrap(bytes, off, len)),
                "wire header must be valid UTF-8");
        return new String(bytes, off, len, StandardCharsets.UTF_8);
    }

    private static void assertNoSurrogateEscape(String json) {
        String lower = json.toLowerCase(Locale.ROOT);
        for (String prefix : new String[] {"\\ud8", "\\ud9", "\\uda", "\\udb", "\\udc", "\\udd", "\\ude", "\\udf"}) {
            assertFalse(lower.contains(prefix), "surrogate escape left in: " + json);
        }
    }

    @Test
    void unpairedHighSurrogateInHeaderValueBecomesReplacementChar() {
        byte[] wire = VesperaBridge.encodeRequest(
                null, "GET", "/x", null, Map.of("x-test", "a\uD83Db"), null);
        String json = headerJson(wire);
        assertTrue(json.contains("\"a" + REPLACEMENT + "b\""), json);
        assertNoSurrogateEscape(json);
    }

    @Test
    void trailingHighSurrogateAndLoneLowSurrogateBecomeReplacementChars() {
        byte[] wire = VesperaBridge.encodeRequest(
                null, "GET", "/p\uDC00", null, Map.of("x-tail", "v\uD800"), null);
        String json = headerJson(wire);
        assertTrue(json.contains("\"/p" + REPLACEMENT + "\""), json);
        assertTrue(json.contains("\"v" + REPLACEMENT + "\""), json);
        assertNoSurrogateEscape(json);
    }

    @Test
    void reversedPairIsTwoReplacementChars() {
        VesperaWireCodec.ExposedByteArrayOutputStream header = VesperaWireCodec.fillHeaderJson(
                null, "GET", "/\uDE00\uD83Dx", null, Map.of());
        String json = strictUtf8(header.backingArray(), 0, header.size());
        assertTrue(json.contains("/" + REPLACEMENT + REPLACEMENT + "x"), json);
    }

    @Test
    void principalAttributesAndAuthoritiesReplaceUnpairedSurrogates() {
        VesperaWireCodec.ExposedByteArrayOutputStream buf =
                new VesperaWireCodec.ExposedByteArrayOutputStream(64);
        VesperaWireCodec.writePrincipal(buf, new VesperaPrincipal(
                "al\uDC00ice", List.of("ROLE_\uD83D"), Map.of("k\uD800", "v\uDFFF")));
        String json = strictUtf8(buf.backingArray(), 0, buf.size());
        assertTrue(json.contains("\"al" + REPLACEMENT + "ice\""), json);
        assertTrue(json.contains("\"ROLE_" + REPLACEMENT + "\""), json);
        assertTrue(json.contains("\"k" + REPLACEMENT + "\":\"v" + REPLACEMENT + "\""), json);
        assertNoSurrogateEscape(json);
    }

    @Test
    void validSurrogatePairStillBecomesFourByteUtf8() {
        // U+1F600 GRINNING FACE = high \uD83D + low \uDE00 — must stay the real
        // 4-byte UTF-8 character (NOT escaped or replaced).
        byte[] wire = VesperaBridge.encodeRequest(
                null, "GET", "/x", null, Map.of("x-emoji", "😀"), null);
        String json = headerJson(wire);
        assertTrue(
                json.contains("😀"),
                "valid surrogate pair must round-trip as the actual character");
    }

    @Test
    void everyShortControlEscapeAndTwoByteUtf8EncodingIsExact() {
        byte[] wire = VesperaBridge.encodeRequest(
                null, "GET", "/x", null,
                Map.of("x-control", "\b\f\r", "x-two-byte", "é"), null);
        String json = headerJson(wire);

        assertTrue(json.contains("\\b\\f\\r"), json);
        assertTrue(json.contains("é"), json);
        assertEquals(-1, json.indexOf('\b'));
        assertEquals(-1, json.indexOf('\f'));
        assertEquals(-1, json.indexOf('\r'));
    }
}
