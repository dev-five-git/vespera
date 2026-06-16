package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.ByteBuffer;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import org.junit.jupiter.api.Test;

/**
 * B3: the manual JSON encoder ({@code VesperaBridge.writeJsonString}, exercised
 * here through {@link VesperaBridge#encodeRequest}) must escape <em>unpaired</em>
 * UTF-16 surrogates as a {@code \\uXXXX} escape instead of emitting an invalid
 * 3-byte UTF-8 sequence — otherwise the wire header is not valid UTF-8 / RFC 8259
 * JSON and the Rust {@code serde_json} side rejects it. No native library needed.
 */
class JsonEncodingSurrogateTest {

    /** Extract the JSON header region and assert it is strictly valid UTF-8. */
    private static String headerJson(byte[] wire) {
        int len = ((wire[0] & 0xFF) << 24)
                | ((wire[1] & 0xFF) << 16)
                | ((wire[2] & 0xFF) << 8)
                | (wire[3] & 0xFF);
        var decoder = StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        assertDoesNotThrow(
                () -> decoder.decode(ByteBuffer.wrap(wire, 4, len)),
                "wire header must be valid UTF-8");
        return new String(wire, 4, len, StandardCharsets.UTF_8);
    }

    @Test
    void unpairedHighSurrogateInHeaderValueIsEscaped() {
        byte[] wire = VesperaBridge.encodeRequest(
                null, "GET", "/x", null, Map.of("x-test", "\uD800"), null);
        String json = headerJson(wire);
        assertTrue(
                json.toLowerCase().contains("\\ud800"),
                "lone high surrogate must be emitted as a \\u escape, got: " + json);
    }

    @Test
    void loneLowSurrogateInPathIsEscaped() {
        byte[] wire = VesperaBridge.encodeRequest(
                null, "GET", "/p\uDC00", null, Map.of(), null);
        String json = headerJson(wire);
        assertTrue(
                json.toLowerCase().contains("\\udc00"),
                "lone low surrogate must be emitted as a \\u escape, got: " + json);
    }

    @Test
    void validSurrogatePairStillBecomesFourByteUtf8() {
        // U+1F600 GRINNING FACE = high \uD83D + low \uDE00 — must stay the real
        // 4-byte UTF-8 character (NOT escaped), unchanged by the B3 fix.
        byte[] wire = VesperaBridge.encodeRequest(
                null, "GET", "/x", null, Map.of("x-emoji", "\uD83D\uDE00"), null);
        String json = headerJson(wire);
        assertTrue(
                json.contains("\uD83D\uDE00"),
                "valid surrogate pair must round-trip as the actual character");
    }
}
