package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;

/** Shared string/canonical-key helpers for {@link WireHeaderReader}. */
final class WireHeaderStringSupport {

    private static final int DIRECT_STRING_SCRATCH_INITIAL = 256;
    private static final int DIRECT_STRING_SCRATCH_MAX = 8 * 1024;
    private static final ThreadLocal<byte[]> DIRECT_STRING_SCRATCH =
            ThreadLocal.withInitial(() -> new byte[DIRECT_STRING_SCRATCH_INITIAL]);

    private static final String[] CANONICAL_KEYS = {
        "content-type", "content-length", "content-encoding",
        "content-disposition", "cache-control", "set-cookie", "location",
        "etag", "date", "vary", "access-control-allow-origin",
        "version", "path", "code", "message",
    };

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

    static String canonicalKey(ByteBuffer buf, int start, int len) {
        for (String key : CANONICAL_KEYS) {
            if (key.length() == len && regionEquals(buf, start, key)) {
                return key;
            }
        }
        return null;
    }

    static boolean regionEquals(ByteBuffer buf, int start, String literal) {
        for (int i = 0; i < literal.length(); i++) {
            if ((buf.get(start + i) & 0xFF) != literal.charAt(i)) {
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
