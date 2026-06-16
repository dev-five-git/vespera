package com.devfive.vespera.bridge;

import org.junit.jupiter.api.Test;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * Pure-Java wire-equivalence tests: {@link VesperaBridge#encodeRequestInto}
 * must produce byte-identical output to {@link VesperaBridge#encodeRequest}
 * for the same inputs.  No native library required.
 */
class EncodeRequestIntoTest {

    private static byte[] drain(ByteBuffer target, int len) {
        byte[] out = new byte[len];
        target.get(0, out);
        return out;
    }

    private static void assertEquivalent(
            String appName, String method, String path, String query,
            Map<String, String> headers, byte[] body) {
        byte[] expected = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);

        ByteBuffer target = ByteBuffer.allocateDirect(expected.length + 16);
        int written = VesperaBridge.encodeRequestInto(
                appName, method, path, query, headers, body, target);

        assertEquals(expected.length, written, "written length");
        assertArrayEquals(expected, drain(target, written),
                "encodeRequestInto must be byte-identical to encodeRequest");
    }

    private static VesperaBridge.HeaderSource sourceFrom(Map<String, String> headers) {
        return sink -> headers.forEach(sink::put);
    }

    private static void assertHeaderSourceEquivalent(
            String appName, String method, String path, String query,
            Map<String, String> headers, byte[] body) {
        byte[] expected = VesperaBridge.encodeRequest(
                appName, method, path, query, headers, body);
        byte[] actual = VesperaBridge.encodeRequest(
                appName, method, path, query, sourceFrom(headers), body);
        assertArrayEquals(expected, actual,
                "HeaderSource encodeRequest must be byte-identical to Map encodeRequest");
    }

    @Test
    void typicalPostWithBodyAndHeaders() {
        assertEquivalent(null, "POST", "/echo", "a=1&b=2",
                Map.of("content-type", "application/json"),
                "{\"k\":42}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void multiAppGetWithoutBody() {
        assertEquivalent("admin", "GET", "/dashboard", null, Map.of(), null);
    }

    @Test
    void emptyBodyAndNullQuery() {
        assertEquivalent(null, "DELETE", "/items/9", null,
                Map.of("x-custom", "v"), new byte[0]);
    }

    @Test
    void binaryBodySurvivesVerbatim() {
        byte[] binary = new byte[257];
        for (int i = 0; i < binary.length; i++) {
            binary[i] = (byte) i;
        }
        assertEquivalent(null, "POST", "/upload", null,
                Map.of("content-type", "application/octet-stream"), binary);
    }

    @Test
    void tooSmallTargetReturnsNegativeRequiredAndWritesNothing() {
        byte[] body = "payload".getBytes(StandardCharsets.UTF_8);
        byte[] expected = VesperaBridge.encodeRequest(null, "POST", "/x", null, Map.of(), body);

        ByteBuffer tiny = ByteBuffer.allocateDirect(8);
        tiny.put(0, (byte) 0x7F); // sentinel byte to prove nothing was written
        int rc = VesperaBridge.encodeRequestInto(null, "POST", "/x", null, Map.of(), body, tiny);

        assertEquals(-expected.length, rc, "must report exact required size, negated");
        assertEquals((byte) 0x7F, tiny.get(0), "target must be untouched on failure");
    }

    @Test
    void heapTargetAlsoSupported() {
        // encodeRequestInto is buffer-kind-agnostic (only the JNI
        // dispatch requires direct buffers).
        byte[] expected = VesperaBridge.encodeRequest(null, "GET", "/h", null, Map.of(), null);
        ByteBuffer heap = ByteBuffer.allocate(expected.length);
        int written = VesperaBridge.encodeRequestInto(null, "GET", "/h", null, Map.of(), null, heap);
        assertEquals(expected.length, written);
        assertTrue(heap.hasArray());
        byte[] out = new byte[written];
        heap.get(0, out);
        assertArrayEquals(expected, out);
    }

    @Test
    void headerSourceEmptyHeadersByteIdentical() {
        assertHeaderSourceEquivalent(null, "GET", "/empty", null, Map.of(), null);
    }

    @Test
    void headerSourceOneHeaderByteIdentical() {
        assertHeaderSourceEquivalent(null, "GET", "/one", null,
                Map.of("accept", "application/json"), null);
    }

    @Test
    void headerSourceSeveralHeadersByteIdentical() {
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("host", "example.test");
        headers.put("content-type", "application/json");
        headers.put("x-custom-trace", "01HV2N3M4P5Q6R7S8T9V0W1X2Y");
        headers.put("accept-encoding", "gzip, br");
        assertHeaderSourceEquivalent(null, "POST", "/several", null,
                headers, "{}".getBytes(StandardCharsets.UTF_8));
    }

    @Test
    void headerSourceSpecialHeaderValuesByteIdentical() {
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("x-quote", "a\"b\\c");
        headers.put("x-control", "line\n tab\t nul\u0000 end");
        headers.put("x-utf8", "안녕 🌙");
        assertHeaderSourceEquivalent(null, "GET", "/special", null, headers, null);
    }

    @Test
    void headerSourceAppNameAndQueryByteIdentical() {
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("accept", "application/json");
        headers.put("x-app", "admin");
        assertHeaderSourceEquivalent(" admin ", "GET", "/dashboard", "q=rust&sort=desc",
                headers, null);
    }

    @Test
    void headerSourceNoAppNameWithQueryByteIdentical() {
        assertHeaderSourceEquivalent(null, "GET", "/search", "q=vespera",
                Map.of("accept", "application/json"), null);
    }
}
