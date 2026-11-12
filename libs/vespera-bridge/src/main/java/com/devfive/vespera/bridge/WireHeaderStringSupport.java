package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;

/**
 * Shared string/canonical-key helpers for {@link WireHeaderReader}.
 *
 * <p><strong>Header-value charset contract (Latin-1).</strong> HTTP header
 * values are byte strings; servlet containers expose them as ISO-8859-1
 * {@code String}s (one byte → one char {@code U+0000–U+00FF}) and write
 * response header chars back the same way. The wire header is UTF-8 JSON, so
 * the bytes travel as those chars:
 * <ul>
 *   <li><em>Request:</em> the proxy forwards the container's string unchanged
 *       (JSON-encoded as UTF-8); Rust maps a value whose chars are all
 *       {@code <= U+00FF} back to one byte per char — the raw bytes the
 *       client sent. A value containing any wider char keeps its UTF-8
 *       encoding (non-servlet producers sending UTF-8 text).</li>
 *   <li><em>Response:</em> Rust emits every value byte as the char with the
 *       same code point (bytes {@code >= 0x80} included — they used to be
 *       dropped), so {@code HttpServletResponse.addHeader} restores the exact
 *       bytes, e.g. a UTF-8 {@code Content-Disposition} filename.</li>
 * </ul>
 * ASCII values are unaffected in both directions. Header names are ASCII
 * tokens and never remapped.
 */
final class WireHeaderStringSupport {

    private static final int DIRECT_STRING_SCRATCH_INITIAL = 256;
    private static final int DIRECT_STRING_SCRATCH_MAX = 8 * 1024;
    private static final ThreadLocal<byte[]> DIRECT_STRING_SCRATCH =
            ThreadLocal.withInitial(() -> new byte[DIRECT_STRING_SCRATCH_INITIAL]);

    /**
     * Canonical wire-header names indexed by their byte length; row {@code n} holds only
     * {@code n}-byte names, so the row index reproduces the old {@code switch (len)} gate.
     * Candidates within a row are distinct literals and therefore mutually exclusive, so
     * iteration order cannot change the result. Rows store the interned literals themselves,
     * so a match hands back the identical {@code String} instance on every call.
     */
    private static final String[][] CANONICAL_BY_LEN = new String[28][];

    static {
        CANONICAL_BY_LEN[4] = new String[] {"etag", "date", "vary", "path", "code"};
        CANONICAL_BY_LEN[7] = new String[] {"version", "message"};
        CANONICAL_BY_LEN[8] = new String[] {"location"};
        CANONICAL_BY_LEN[10] = new String[] {"set-cookie"};
        CANONICAL_BY_LEN[12] = new String[] {"content-type"};
        CANONICAL_BY_LEN[13] = new String[] {"cache-control"};
        CANONICAL_BY_LEN[14] = new String[] {"content-length"};
        CANONICAL_BY_LEN[16] = new String[] {"content-encoding"};
        CANONICAL_BY_LEN[19] = new String[] {"content-disposition"};
        CANONICAL_BY_LEN[27] = new String[] {"access-control-allow-origin"};
    }

    private WireHeaderStringSupport() {}

    static void clearCurrentThreadBuffers() {
        DIRECT_STRING_SCRATCH.remove();
    }

    static String readAsciiString(ByteBuffer buf, int start, int len) {
        if (buf.hasArray()) {
            return new String(
                    buf.array(),
                    buf.arrayOffset() + start,
                    len,
                    StandardCharsets.US_ASCII);
        }
        if (len <= DIRECT_STRING_SCRATCH_MAX) {
            byte[] scratch = directStringScratch(len);
            buf.get(start, scratch, 0, len);
            return new String(scratch, 0, len, StandardCharsets.US_ASCII);
        }
        byte[] tmp = new byte[len];
        buf.get(start, tmp, 0, len);
        return new String(tmp, StandardCharsets.US_ASCII);
    }

    static String readAsciiString(byte[] buf, int start, int len) {
        return new String(buf, start, len, StandardCharsets.US_ASCII);
    }

    static String canonicalKey(ByteBuffer buf, int start, int len) {
        String[] candidates = canonicalCandidates(len);
        if (candidates == null) return null;
        for (String candidate : candidates) {
            if (regionEquals(buf, start, candidate)) return candidate;
        }
        return null;
    }

    static String canonicalKey(byte[] buf, int start, int len) {
        String[] candidates = canonicalCandidates(len);
        if (candidates == null) return null;
        for (String candidate : candidates) {
            if (regionEquals(buf, start, candidate)) return candidate;
        }
        return null;
    }

    private static String[] canonicalCandidates(int len) {
        return (len >= 0 && len < CANONICAL_BY_LEN.length) ? CANONICAL_BY_LEN[len] : null;
    }

    static boolean regionEquals(ByteBuffer buf, int start, String literal) {
        for (int i = 0; i < literal.length(); i++) {
            if ((buf.get(start + i) & 0xFF) != literal.charAt(i)) {
                return false;
            }
        }
        return true;
    }

    static boolean regionEquals(byte[] buf, int start, String literal) {
        for (int i = 0; i < literal.length(); i++) {
            if ((buf[start + i] & 0xFF) != literal.charAt(i)) {
                return false;
            }
        }
        return true;
    }

    private static byte[] directStringScratch(int required) {
        byte[] scratch = DIRECT_STRING_SCRATCH.get();
        if (scratch.length < required) {
            scratch = new byte[Math.min(
                    DIRECT_STRING_SCRATCH_MAX,
                    Math.max(required, scratch.length * 2))];
            DIRECT_STRING_SCRATCH.set(scratch);
        }
        return scratch;
    }
}
