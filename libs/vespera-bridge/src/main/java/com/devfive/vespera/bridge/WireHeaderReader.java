package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.util.function.BiConsumer;
import java.util.function.IntConsumer;

/**
 * Zero-copy reader for the response wire header, used by the DIRECT
 * dispatch path to apply {@code status} + {@code headers} straight from
 * the pooled direct {@link ByteBuffer} — no intermediate {@code byte[]}
 * copy, no {@code DecodedResponse} object graph (maps / metadata / body
 * views), no per-call allocation beyond the header-value {@link String}s
 * the servlet API itself requires.
 *
 * <p>Reads bytes via absolute {@link ByteBuffer#get(int)} so a
 * direct buffer (no backing array, which {@code Jackson.createParser}
 * cannot consume without a copy) is parsed in place.
 *
 * <p>Not a general JSON validator: it assumes the well-formed,
 * fixed-schema header produced by the Rust {@code serde_json} side. Only
 * the quote / backslash / control escapes and raw UTF-8 that
 * {@code serde_json} emits are handled. Unknown fields ({@code v},
 * {@code metadata}, {@code validation_errors}, …) are skipped.
 */
final class WireHeaderReader {

    private final ByteBuffer buf;
    private int pos;
    private final int end;

    private WireHeaderReader(ByteBuffer buf, int off, int len) {
        this.buf = buf;
        this.pos = off;
        this.end = off + len;
    }

    /**
     * Parse the header JSON in {@code buf[off .. off+len]} and apply it:
     * {@code statusSink} is invoked exactly once (default {@code 500}
     * when the {@code status} field is absent, matching
     * {@code decodeResponse}); {@code headerSink} is invoked once per
     * header value (multiple times for multi-valued headers such as
     * {@code set-cookie}).
     */
    static void apply(
            ByteBuffer buf,
            int off,
            int len,
            IntConsumer statusSink,
            BiConsumer<String, String> headerSink) {
        WireHeaderReader r = new WireHeaderReader(buf, off, len);
        int status = 500;
        if (r.peek() == '{') {
            r.beginObject();
            String name;
            while ((name = r.nextKey()) != null) {
                switch (name) {
                    case "status" -> status = r.readInt();
                    case "headers" -> {
                        if (r.isObjectStart()) {
                            r.beginObject();
                            String k;
                            while ((k = r.nextKey()) != null) {
                                if (r.isArrayStart()) {
                                    r.beginArray();
                                    while (r.hasNextElement()) {
                                        headerSink.accept(k, r.readString());
                                    }
                                } else {
                                    headerSink.accept(k, r.readString());
                                }
                            }
                        } else {
                            r.skipValue();
                        }
                    }
                    default -> r.skipValue();
                }
            }
        }
        statusSink.accept(status);
    }

    private void skipWs() {
        while (pos < end) {
            int c = buf.get(pos) & 0xFF;
            if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                pos++;
            } else {
                break;
            }
        }
    }

    private int cur() {
        return pos < end ? buf.get(pos) & 0xFF : -1;
    }

    int peek() {
        skipWs();
        return cur();
    }

    private IllegalArgumentException err(String what) {
        return new IllegalArgumentException("wire header JSON: " + what + " at offset " + pos);
    }

    private void expect(char c) {
        skipWs();
        if (cur() != c) {
            throw err("expected '" + c + "'");
        }
        pos++;
    }

    void beginObject() {
        expect('{');
    }

    /** Next member key, or {@code null} at object end (stateless across nesting). */
    String nextKey() {
        skipWs();
        int c = cur();
        if (c == ',') {
            pos++;
            skipWs();
            c = cur();
        }
        if (c == '}') {
            pos++;
            return null;
        }
        String key = readString();
        expect(':');
        return key;
    }

    void beginArray() {
        expect('[');
    }

    boolean hasNextElement() {
        skipWs();
        int c = cur();
        if (c == ',') {
            pos++;
            skipWs();
            c = cur();
        }
        if (c == ']') {
            pos++;
            return false;
        }
        return true;
    }

    boolean isObjectStart() {
        return peek() == '{';
    }

    boolean isArrayStart() {
        return peek() == '[';
    }

    String readString() {
        skipWs();
        if (cur() != '"') {
            throw err("expected string");
        }
        pos++;
        StringBuilder sb = new StringBuilder();
        while (pos < end) {
            int b = buf.get(pos++) & 0xFF;
            if (b == '"') {
                return sb.toString();
            }
            if (b == '\\') {
                if (pos >= end) {
                    throw err("dangling escape");
                }
                int e = buf.get(pos++) & 0xFF;
                switch (e) {
                    case '"' -> sb.append('"');
                    case '\\' -> sb.append('\\');
                    case '/' -> sb.append('/');
                    case 'b' -> sb.append('\b');
                    case 'f' -> sb.append('\f');
                    case 'n' -> sb.append('\n');
                    case 'r' -> sb.append('\r');
                    case 't' -> sb.append('\t');
                    case 'u' -> sb.append(readHex4());
                    default -> throw err("bad escape");
                }
            } else if (b < 0x80) {
                sb.append((char) b);
            } else if (b < 0xE0) {
                sb.append((char) (((b & 0x1F) << 6) | nextCont()));
            } else if (b < 0xF0) {
                sb.append((char) (((b & 0x0F) << 12) | (nextCont() << 6) | nextCont()));
            } else {
                int cp = ((b & 0x07) << 18) | (nextCont() << 12) | (nextCont() << 6) | nextCont();
                sb.appendCodePoint(cp);
            }
        }
        throw err("unterminated string");
    }

    private int nextCont() {
        if (pos >= end) {
            throw err("truncated UTF-8");
        }
        return buf.get(pos++) & 0x3F;
    }

    private char readHex4() {
        if (pos + 4 > end) {
            throw err("truncated unicode escape");
        }
        int v = 0;
        for (int k = 0; k < 4; k++) {
            int d = buf.get(pos++) & 0xFF;
            int h;
            if (d >= '0' && d <= '9') {
                h = d - '0';
            } else if (d >= 'a' && d <= 'f') {
                h = d - 'a' + 10;
            } else if (d >= 'A' && d <= 'F') {
                h = d - 'A' + 10;
            } else {
                throw err("bad hex digit");
            }
            v = (v << 4) | h;
        }
        return (char) v;
    }

    int readInt() {
        skipWs();
        int start = pos;
        boolean neg = cur() == '-';
        if (neg) {
            pos++;
        }
        boolean any = false;
        long v = 0;
        while (pos < end) {
            int d = buf.get(pos) & 0xFF;
            if (d < '0' || d > '9') {
                break;
            }
            v = v * 10 + (d - '0');
            pos++;
            any = true;
        }
        if (pos < end) {
            int c = cur();
            if (c == '.' || c == 'e' || c == 'E') {
                skipNumberTail();
            }
        }
        if (!any) {
            pos = start;
            throw err("expected number");
        }
        return (int) (neg ? -v : v);
    }

    private void skipNumberTail() {
        while (pos < end) {
            int d = buf.get(pos) & 0xFF;
            if ((d >= '0' && d <= '9') || d == '.' || d == 'e' || d == 'E' || d == '+' || d == '-') {
                pos++;
            } else {
                break;
            }
        }
    }

    void skipValue() {
        int c = peek();
        switch (c) {
            case '{' -> {
                beginObject();
                while (nextKey() != null) {
                    skipValue();
                }
            }
            case '[' -> {
                beginArray();
                while (hasNextElement()) {
                    skipValue();
                }
            }
            case '"' -> readString();
            case 't', 'f', 'n' -> skipLiteral();
            default -> {
                if (c == '-' || (c >= '0' && c <= '9')) {
                    readInt();
                } else {
                    throw err("unexpected value");
                }
            }
        }
    }

    private void skipLiteral() {
        while (pos < end) {
            int d = buf.get(pos) & 0xFF;
            if (d >= 'a' && d <= 'z') {
                pos++;
            } else {
                break;
            }
        }
    }
}
