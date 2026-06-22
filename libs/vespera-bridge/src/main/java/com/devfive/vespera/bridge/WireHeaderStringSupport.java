package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;

/** Shared string/canonical-key helpers for {@link WireHeaderReader}. */
final class WireHeaderStringSupport {

    private static final int DIRECT_STRING_SCRATCH_INITIAL = 256;
    private static final int DIRECT_STRING_SCRATCH_MAX = 8 * 1024;
    private static final ThreadLocal<byte[]> DIRECT_STRING_SCRATCH =
            ThreadLocal.withInitial(() -> new byte[DIRECT_STRING_SCRATCH_INITIAL]);

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
        return switch (len) {
            case 4 -> canonicalKeyLen4(buf, start);
            case 7 -> canonicalKeyLen7(buf, start);
            case 8 -> regionEquals(buf, start, "location") ? "location" : null;
            case 10 -> regionEquals(buf, start, "set-cookie") ? "set-cookie" : null;
            case 12 -> regionEquals(buf, start, "content-type") ? "content-type" : null;
            case 13 -> regionEquals(buf, start, "cache-control") ? "cache-control" : null;
            case 14 -> regionEquals(buf, start, "content-length") ? "content-length" : null;
            case 16 -> regionEquals(buf, start, "content-encoding") ? "content-encoding" : null;
            case 19 -> regionEquals(buf, start, "content-disposition") ? "content-disposition" : null;
            case 27 -> regionEquals(buf, start, "access-control-allow-origin")
                    ? "access-control-allow-origin" : null;
            default -> null;
        };
    }

    static String canonicalKey(byte[] buf, int start, int len) {
        return switch (len) {
            case 4 -> canonicalKeyLen4(buf, start);
            case 7 -> canonicalKeyLen7(buf, start);
            case 8 -> regionEquals(buf, start, "location") ? "location" : null;
            case 10 -> regionEquals(buf, start, "set-cookie") ? "set-cookie" : null;
            case 12 -> regionEquals(buf, start, "content-type") ? "content-type" : null;
            case 13 -> regionEquals(buf, start, "cache-control") ? "cache-control" : null;
            case 14 -> regionEquals(buf, start, "content-length") ? "content-length" : null;
            case 16 -> regionEquals(buf, start, "content-encoding") ? "content-encoding" : null;
            case 19 -> regionEquals(buf, start, "content-disposition") ? "content-disposition" : null;
            case 27 -> regionEquals(buf, start, "access-control-allow-origin")
                    ? "access-control-allow-origin" : null;
            default -> null;
        };
    }

    private static String canonicalKeyLen4(ByteBuffer buf, int start) {
        if (regionEquals(buf, start, "etag")) return "etag";
        if (regionEquals(buf, start, "date")) return "date";
        if (regionEquals(buf, start, "vary")) return "vary";
        if (regionEquals(buf, start, "path")) return "path";
        if (regionEquals(buf, start, "code")) return "code";
        return null;
    }

    private static String canonicalKeyLen4(byte[] buf, int start) {
        if (regionEquals(buf, start, "etag")) return "etag";
        if (regionEquals(buf, start, "date")) return "date";
        if (regionEquals(buf, start, "vary")) return "vary";
        if (regionEquals(buf, start, "path")) return "path";
        if (regionEquals(buf, start, "code")) return "code";
        return null;
    }

    private static String canonicalKeyLen7(ByteBuffer buf, int start) {
        if (regionEquals(buf, start, "version")) return "version";
        if (regionEquals(buf, start, "message")) return "message";
        return null;
    }

    private static String canonicalKeyLen7(byte[] buf, int start) {
        if (regionEquals(buf, start, "version")) return "version";
        if (regionEquals(buf, start, "message")) return "message";
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
