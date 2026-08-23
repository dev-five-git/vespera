package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.lang.reflect.Constructor;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import org.junit.jupiter.api.Test;

class WireHeaderReaderBranchTest {

    private static WireHeaderReader reader(String json) throws Exception {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        Constructor<WireHeaderReader> constructor =
                WireHeaderReader.class.getDeclaredConstructor(byte[].class, int.class, int.class);
        constructor.setAccessible(true);
        return constructor.newInstance(bytes, 0, bytes.length);
    }

    private static WireHeaderReader.Decoded decodeBoth(String json) {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
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

    private static void assertDecodeFailure(String json, String expectedMessage) {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        IllegalArgumentException heap = assertThrows(
                IllegalArgumentException.class,
                () -> WireHeaderReader.decode(bytes, 0, bytes.length));
        assertEquals(expectedMessage, heap.getMessage());

        ByteBuffer direct = ByteBuffer.allocateDirect(bytes.length);
        direct.put(bytes);
        IllegalArgumentException directError = assertThrows(
                IllegalArgumentException.class,
                () -> WireHeaderReader.decode(direct, 0, bytes.length));
        assertEquals(expectedMessage, directError.getMessage());
    }

    @Test
    void rootMatcherCoversSameLengthMissesAndEscapeAtEndOfInput() {
        WireHeaderReader.Decoded decoded = decodeBoth(
                "{\"metadatx\":null,\"validation_errorx\":null,\"status\":200}");
        assertEquals(200, decoded.status);
        assertEquals(Map.of(), decoded.metadata);

        String danglingEscape = "{\"x\\";
        assertDecodeFailure(
                danglingEscape,
                "wire header JSON: unterminated string at offset " + danglingEscape.length());
    }

    @Test
    void acceptsHighestValidThreeAndFourByteUtf8Scalars() {
        String highestBeforeSurrogates = "\uD7FF";
        String highestScalar = new String(Character.toChars(0x10FFFF));
        WireHeaderReader.Decoded decoded = decodeBoth(
                "{\"headers\":{\"three\":\"" + highestBeforeSurrogates
                        + "\",\"four\":\"" + highestScalar + "\"}}");

        assertEquals(highestBeforeSurrogates, decoded.headers.get("three"));
        assertEquals(highestScalar, decoded.headers.get("four"));
    }

    @Test
    void primitiveReaderCoversUppercaseAndNegativeExponentsAndPunctuationRejection() {
        WireHeaderReader.Decoded decoded = decodeBoth(
                "{\"validation_errors\":[{\"upper\":1E2,\"negative\":1e-2}]}");
        assertEquals(100.0, decoded.validationErrors.get(0).get("upper"));
        assertEquals(0.01, decoded.validationErrors.get(0).get("negative"));

        String punctuation = "{\"validation_errors\":[{\"x\"::}]}";
        assertDecodeFailure(
                punctuation,
                "wire header JSON: unexpected primitive value at offset "
                        + (punctuation.indexOf("::", punctuation.indexOf("\"x\"")) + 1));
    }

    @Test
    void numericReadersConsumeDigitsExactlyToEndOfInput() throws Exception {
        assertEquals(1L, reader("1").readPrimitiveValue());
        assertEquals(200, reader("200").readStatusCode());
    }

    @Test
    void unicodeHighSurrogateRequiresACompleteFollowingUnicodeEscape() throws Exception {
        String noFollowingEscape = "\"\\uD800";
        IllegalArgumentException truncated = assertThrows(
                IllegalArgumentException.class, () -> reader(noFollowingEscape).readString());
        assertEquals(
                "wire header JSON: unpaired unicode surrogate at offset "
                        + noFollowingEscape.length(),
                truncated.getMessage());

        String wrongEscapeType = "\"\\uD800\\x0000\"";
        IllegalArgumentException wrongType = assertThrows(
                IllegalArgumentException.class, () -> reader(wrongEscapeType).readString());
        assertEquals(
                "wire header JSON: unpaired unicode surrogate at offset 7",
                wrongType.getMessage());
    }

    @Test
    void statusRejectsUppercaseExponentAtTheDigitBoundary() {
        String json = "{\"status\":2E2}";
        assertDecodeFailure(
                json,
                "wire header JSON: status must be an integer (no fraction or exponent) at offset "
                        + json.indexOf('E'));
    }

    @Test
    void skippedNumbersAcceptUppercaseExponentAndBothExponentSigns() {
        WireHeaderReader.Decoded decoded = decodeBoth(
                "{\"positive\":1E+2,\"negative\":1e-2,\"status\":204}");
        assertEquals(204, decoded.status);
    }

    @Test
    void skippedStringEndingInEscapeReportsExactEndOffset() {
        String json = "{\"unknown\":\"abc\\";
        assertDecodeFailure(
                json,
                "wire header JSON: unterminated string at offset " + json.length());
    }

    @Test
    void skippedNestedStringEndingInEscapeReportsUnterminatedContainer() {
        String json = "{\"unknown\":{\"x\":\"abc\\";
        assertDecodeFailure(
                json,
                "wire header JSON: unterminated container at offset " + json.length());
    }

    private static final String HEADER_VALUE_PREFIX = "{\"headers\":{\"k\":\"";

    private static void assertDecodeFailure(byte[] json, String expectedMessage) {
        IllegalArgumentException heap = assertThrows(
                IllegalArgumentException.class,
                () -> WireHeaderReader.decode(json, 0, json.length));
        assertEquals(expectedMessage, heap.getMessage());

        ByteBuffer direct = ByteBuffer.allocateDirect(json.length);
        direct.put(json);
        IllegalArgumentException directError = assertThrows(
                IllegalArgumentException.class,
                () -> WireHeaderReader.decode(direct, 0, json.length));
        assertEquals(expectedMessage, directError.getMessage());
    }

    private static byte[] headerValueBytes(int... rawValueBytes) {
        byte[] prefix = HEADER_VALUE_PREFIX.getBytes(StandardCharsets.UTF_8);
        byte[] suffix = "\"}}".getBytes(StandardCharsets.UTF_8);
        byte[] out = new byte[prefix.length + rawValueBytes.length + suffix.length];
        System.arraycopy(prefix, 0, out, 0, prefix.length);
        for (int i = 0; i < rawValueBytes.length; i++) {
            out[prefix.length + i] = (byte) rawValueBytes[i];
        }
        System.arraycopy(suffix, 0, out, prefix.length + rawValueBytes.length, suffix.length);
        return out;
    }

    @Test
    void threeByteUtf8RejectsOverlongAndSurrogateEncodings() {
        // The lead byte and one continuation byte are consumed before the
        // check throws, so the reported offset is the lead index plus two.
        String expected = "wire header JSON: bad UTF-8 at offset "
                + (HEADER_VALUE_PREFIX.length() + 2);

        assertDecodeFailure(headerValueBytes(0xE0, 0x80, 0x80), expected);
        assertDecodeFailure(headerValueBytes(0xED, 0xA0, 0x80), expected);

        WireHeaderReader.Decoded decoded = decodeBoth(HEADER_VALUE_PREFIX + "\u0800\"}}");
        assertEquals("\u0800", decoded.headers.get("k"));
    }

    @Test
    void highSurrogateFollowedByAPlainCharacterIsRejected() throws Exception {
        String plainFollower = "\"\\uD800ABCDEFGH\"";
        IllegalArgumentException error = assertThrows(
                IllegalArgumentException.class, () -> reader(plainFollower).readString());
        assertEquals(
                "wire header JSON: unpaired unicode surrogate at offset 7",
                error.getMessage());
    }

    @Test
    void negativeNumbersAreReadAsPrimitiveValues() {
        WireHeaderReader.Decoded decoded =
                decodeBoth("{\"validation_errors\":[{\"neg\":-5}]}");
        assertEquals(-5L, decoded.validationErrors.get(0).get("neg"));
    }

    @Test
    void negativeNumbersAreSkippedForUnknownKeys() {
        WireHeaderReader.Decoded decoded = decodeBoth("{\"unknown\":-5,\"status\":200}");
        assertEquals(200, decoded.status);
    }

    @Test
    void primitiveValueRejectsACharacterBelowTheDigitRange() {
        String json = "{\"validation_errors\":[{\"x\":+}]}";
        assertDecodeFailure(
                json,
                "wire header JSON: unexpected primitive value at offset " + json.indexOf('+'));
    }

    @Test
    void skippedValueRejectsACharacterBelowTheDigitRange() {
        String json = "{\"unknown\":+,\"status\":200}";
        assertDecodeFailure(
                json,
                "wire header JSON: unexpected value at offset " + json.indexOf('+'));
    }

    @Test
    void truncatedSkippedLiteralReportsItsStartingOffset() {
        String json = "{\"unknown\":tru";
        assertDecodeFailure(
                json,
                "wire header JSON: expected true at offset " + json.indexOf("tru"));
    }
}
