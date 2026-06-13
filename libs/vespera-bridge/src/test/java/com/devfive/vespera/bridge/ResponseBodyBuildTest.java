package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;
import java.lang.management.ManagementFactory;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import org.junit.jupiter.api.Test;
import org.springframework.http.HttpHeaders;

/**
 * Lever 1 gate: the controller builds the response body straight from the wire
 * buffer ({@code Arrays.copyOfRange(wire, bodyOff, end)}) instead of {@code
 * decoded.bodyBytes()}. Since the controller now unifies on {@code
 * ResponseEntity<byte[]>} for every content type, the text helpers below
 * ({@code new String(wire, off, len)}) remain as a byte-identity proof of the
 * extraction offsets across the content/charset matrix — they are no longer the
 * controller's delivery path, which slices to {@code byte[]} uniformly and so
 * drops both the intermediate {@code byte[]} and the prior text-only UTF-8
 * decode→re-encode round-trip.
 */
class ResponseBodyBuildTest {

    /** Assemble a wire response {@code [u32 len | header | body]}. */
    private static byte[] wire(String contentType, byte[] body) {
        String header =
                contentType == null
                        ? "{\"v\":1,\"status\":200,\"headers\":{},\"metadata\":{\"version\":\"0.0.0\"}}"
                        : "{\"v\":1,\"status\":200,\"headers\":{\"content-type\":\""
                                + contentType
                                + "\"},\"metadata\":{\"version\":\"0.0.0\"}}";
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

    // OLD: new String(decoded.bodyBytes(), UTF_8). NEW: new String(wire, off, len).
    private static void assertTextEquivalent(byte[] body) {
        byte[] w = wire("application/json", body);
        DecodedResponse d = VesperaBridge.decodeResponse(w);
        String oldStr = new String(d.bodyBytes(), StandardCharsets.UTF_8);
        int bodyLen = d.body().remaining();
        int bodyOff = w.length - bodyLen;
        String newStr = new String(w, bodyOff, bodyLen, StandardCharsets.UTF_8);
        assertEquals(oldStr, newStr, "text body extraction must match the bodyBytes() path");
    }

    // OLD: decoded.bodyBytes(). NEW: Arrays.copyOfRange(wire, off, end).
    private static void assertBinaryEquivalent(byte[] body) {
        byte[] w = wire("application/octet-stream", body);
        DecodedResponse d = VesperaBridge.decodeResponse(w);
        byte[] oldB = d.bodyBytes();
        int bodyLen = d.body().remaining();
        int bodyOff = w.length - bodyLen;
        byte[] newB = Arrays.copyOfRange(w, bodyOff, w.length);
        assertArrayEquals(oldB, newB, "binary body extraction must match the bodyBytes() path");
        assertArrayEquals(body, newB, "binary body must round-trip exactly");
    }

    @Test
    void textBodyMatrixIsByteIdentical() {
        assertTextEquivalent("{\"ok\":true}".getBytes(StandardCharsets.UTF_8));
        assertTextEquivalent("plain ascii".getBytes(StandardCharsets.UTF_8));
        assertTextEquivalent("café — naïve — 日本語".getBytes(StandardCharsets.UTF_8));
        // 4-byte codepoint (emoji) — the multi-byte boundary case Metis flagged.
        assertTextEquivalent("ok\uD83D\uDE80end".getBytes(StandardCharsets.UTF_8));
        assertTextEquivalent(new byte[0]); // empty
    }

    @Test
    void binaryBodyMatrixIsByteIdentical() {
        byte[] allBytes = new byte[256];
        for (int i = 0; i < 256; i++) {
            allBytes[i] = (byte) i;
        }
        assertBinaryEquivalent(allBytes);
        assertBinaryEquivalent(new byte[0]); // empty
        byte[] big = new byte[64 * 1024];
        new java.util.Random(7).nextBytes(big);
        assertBinaryEquivalent(big);
    }

    @Test
    void isoLatin1BytesRoundTripViaUtf8DecodeUnchanged() {
        // The controller decodes text as UTF-8 regardless of the charset
        // parameter (pre-existing behavior). Confirm the new path preserves
        // exactly that — same bytes in, same String out as the old path.
        byte[] iso = {(byte) 0xE9, (byte) 0xE8, 'a', 'b'}; // é è in ISO-8859-1
        byte[] w = wire("text/plain; charset=ISO-8859-1", iso);
        DecodedResponse d = VesperaBridge.decodeResponse(w);
        String oldStr = new String(d.bodyBytes(), StandardCharsets.UTF_8);
        int bodyLen = d.body().remaining();
        String newStr = new String(w, w.length - bodyLen, bodyLen, StandardCharsets.UTF_8);
        assertEquals(oldStr, newStr);
    }

    /** Allocation saving (bytes/op) — OLD bodyBytes()+String vs NEW direct String. */
    @Test
    void allocationSavingScalesWithBodySize() throws Exception {
        com.sun.management.ThreadMXBean tmx =
                (com.sun.management.ThreadMXBean) ManagementFactory.getThreadMXBean();
        long tid = Thread.currentThread().getId();
        StringBuilder report = new StringBuilder();
        for (int kb : new int[] {1, 64, 1024}) {
            byte[] body = new byte[kb * 1024];
            new java.util.Random(1).nextBytes(body);
            // keep it valid-ish text by masking to ASCII so both paths decode identically
            for (int i = 0; i < body.length; i++) {
                body[i] = (byte) (body[i] & 0x7F);
            }
            byte[] w = wire("application/json", body);

            int warm = 2000;
            int iters = 20000;
            long blackhole = 0;
            for (int i = 0; i < warm; i++) {
                blackhole += oldText(w);
                blackhole += newText(w);
            }
            long b0 = tmx.getThreadAllocatedBytes(tid);
            for (int i = 0; i < iters; i++) blackhole += oldText(w);
            long oldBytes = (tmx.getThreadAllocatedBytes(tid) - b0) / iters;
            long b1 = tmx.getThreadAllocatedBytes(tid);
            for (int i = 0; i < iters; i++) blackhole += newText(w);
            long newBytes = (tmx.getThreadAllocatedBytes(tid) - b1) / iters;
            report.append(
                    String.format(
                            "VESPERA_L1ALLOC body_kb=%d old_bytes=%d new_bytes=%d saved=%d (bh %d)%n",
                            kb, oldBytes, newBytes, oldBytes - newBytes, blackhole & 1));
        }
        Files.writeString(Path.of(System.getProperty("java.io.tmpdir"), "vespera_l1alloc.txt"), report);
    }

    private static int oldText(byte[] w) {
        DecodedResponse d = VesperaBridge.decodeResponse(w);
        return new String(d.bodyBytes(), StandardCharsets.UTF_8).length();
    }

    private static int newText(byte[] w) {
        DecodedResponse d = VesperaBridge.decodeResponse(w);
        int bodyLen = d.body().remaining();
        return new String(w, w.length - bodyLen, bodyLen, StandardCharsets.UTF_8).length();
    }

    // ---- Lever 2: lean status+headers parse (WireHeaderReader) vs decodeResponse graph ----

    private static int headerLen(byte[] w) {
        return ((w[0] & 0xFF) << 24) | ((w[1] & 0xFF) << 16) | ((w[2] & 0xFF) << 8) | (w[3] & 0xFF);
    }

    /** OLD: decodeResponse graph → iterate headers map into HttpHeaders. */
    private static HttpHeaders oldHeaders(byte[] w) {
        DecodedResponse d = VesperaBridge.decodeResponse(w);
        HttpHeaders h = new HttpHeaders();
        for (var e : d.headers().entrySet()) {
            Object v = e.getValue();
            if (v instanceof java.util.List<?> list) {
                for (Object x : list) {
                    h.add(e.getKey(), String.valueOf(x));
                }
            } else if (v != null) {
                h.set(e.getKey(), String.valueOf(v));
            }
        }
        return h;
    }

    /** NEW: lean WireHeaderReader straight into HttpHeaders. */
    private static HttpHeaders leanHeaders(byte[] w, int[] status) {
        HttpHeaders h = new HttpHeaders();
        WireHeaderReader.apply(
                java.nio.ByteBuffer.wrap(w), 4, headerLen(w), s -> status[0] = s, h::add);
        return h;
    }

    @Test
    void leanStatusAndHeadersMatchDecodeResponse() {
        // single-value header
        byte[] w1 = wire("application/json", "{\"x\":1}".getBytes(StandardCharsets.UTF_8));
        DecodedResponse d1 = VesperaBridge.decodeResponse(w1);
        int[] s1 = {-1};
        assertEquals(d1.status(), leanHeaders(w1, s1) == null ? -1 : s1[0]);
        assertEquals(oldHeaders(w1), leanHeaders(w1, new int[1]));
        // multi-value (set-cookie) + status
        String hdr =
                "{\"v\":1,\"status\":201,\"headers\":{\"set-cookie\":[\"a=1\",\"b=2\"],"
                        + "\"content-type\":\"application/json\"},\"metadata\":{\"version\":\"x\"}}";
        byte[] hb = hdr.getBytes(StandardCharsets.UTF_8);
        byte[] w2 = new byte[4 + hb.length];
        w2[0] = (byte) (hb.length >>> 24);
        w2[1] = (byte) (hb.length >>> 16);
        w2[2] = (byte) (hb.length >>> 8);
        w2[3] = (byte) hb.length;
        System.arraycopy(hb, 0, w2, 4, hb.length);
        int[] s2 = {-1};
        HttpHeaders lean2 = leanHeaders(w2, s2);
        assertEquals(201, s2[0]);
        assertEquals(oldHeaders(w2), lean2);
    }

    /** OLD full response build (decodeResponse graph + bodyBytes+String). */
    private static int oldFull(byte[] w) {
        DecodedResponse d = VesperaBridge.decodeResponse(w);
        HttpHeaders h = new HttpHeaders();
        for (var e : d.headers().entrySet()) {
            if (e.getValue() != null) {
                h.add(e.getKey(), String.valueOf(e.getValue()));
            }
        }
        return d.status() + h.size() + new String(d.bodyBytes(), StandardCharsets.UTF_8).length();
    }

    /**
     * NEW full response build (lean reader + body-from-wire) —
     * buildResponseEntityFromWire logic. Since the controller now unifies
     * on {@code ResponseEntity<byte[]>} for every content type (dropping
     * the text-only {@code new String} branch and its UTF-8
     * decode→re-encode round-trip), the body is modelled as the
     * {@code Arrays.copyOfRange} slice the controller actually returns.
     */
    private static int newFull(byte[] w) {
        int hl = headerLen(w);
        HttpHeaders h = new HttpHeaders();
        int[] st = {500};
        WireHeaderReader.apply(java.nio.ByteBuffer.wrap(w), 4, hl, s -> st[0] = s, h::add);
        int bodyOff = 4 + hl;
        return st[0] + h.size() + Arrays.copyOfRange(w, bodyOff, w.length).length;
    }

    @Test
    void combinedAllocationSaving() throws Exception {
        com.sun.management.ThreadMXBean tmx =
                (com.sun.management.ThreadMXBean) ManagementFactory.getThreadMXBean();
        long tid = Thread.currentThread().getId();
        StringBuilder report = new StringBuilder();
        for (int kb : new int[] {0, 1, 64}) {
            byte[] body = new byte[kb * 1024];
            for (int i = 0; i < body.length; i++) {
                body[i] = (byte) ('a' + (i % 26));
            }
            byte[] w = wire("application/json", body);
            int warm = 2000;
            int iters = 20000;
            long bh = 0;
            for (int i = 0; i < warm; i++) {
                bh += oldFull(w);
                bh += newFull(w);
            }
            long b0 = tmx.getThreadAllocatedBytes(tid);
            for (int i = 0; i < iters; i++) bh += oldFull(w);
            long oldB = (tmx.getThreadAllocatedBytes(tid) - b0) / iters;
            long b1 = tmx.getThreadAllocatedBytes(tid);
            for (int i = 0; i < iters; i++) bh += newFull(w);
            long newB = (tmx.getThreadAllocatedBytes(tid) - b1) / iters;
            report.append(
                    String.format(
                            "VESPERA_L2ALLOC body_kb=%d old_bytes=%d new_bytes=%d saved=%d (bh %d)%n",
                            kb, oldB, newB, oldB - newB, bh & 1));
        }
        Files.writeString(Path.of(System.getProperty("java.io.tmpdir"), "vespera_l2alloc.txt"), report);
    }
}
