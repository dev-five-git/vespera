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
            int key;
            while ((key = r.nextRootKey()) != KEY_END) {
                switch (key) {
                    case KEY_STATUS -> status = r.readInt();
                    case KEY_HEADERS -> {
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
                    // KEY_OTHER: "v", "metadata", "validation_errors", … —
                    // matched by bytes, value skipped, never materialised.
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

    // Root-member-key codes for the allocation-free root-key matcher used
    // by apply(): the only root keys the reader acts on are "status" and
    // "headers"; every other key ("v", "metadata", "validation_errors", …)
    // is matched by length+bytes and its value skipped — never materialised
    // as a String.
    private static final int KEY_END = -2;
    private static final int KEY_OTHER = -1;
    private static final int KEY_STATUS = 0;
    private static final int KEY_HEADERS = 1;

    /**
     * Advance past the next root member key WITHOUT allocating a String for
     * it, returning a {@code KEY_*} code ({@code KEY_END} at object end).
     * The allocation-free counterpart of {@link #nextKey()} for the fixed
     * root schema; header keys (delivered to the sink) still use
     * {@link #nextKey()}.
     */
    int nextRootKey() {
        skipWs();
        int c = cur();
        if (c == ',') {
            pos++;
            skipWs();
            c = cur();
        }
        if (c == '}') {
            pos++;
            return KEY_END;
        }
        int code = matchRootKey();
        expect(':');
        return code;
    }

    /**
     * Consume a quoted root key, returning {@code KEY_STATUS} /
     * {@code KEY_HEADERS} when its bytes equal those literals, else
     * {@code KEY_OTHER} — all without allocating.  An escaped key (never
     * emitted for the fixed root field names) is consumed and reported as
     * {@code KEY_OTHER}.
     */
    private int matchRootKey() {
        skipWs();
        if (cur() != '"') {
            throw err("expected string");
        }
        pos++;
        int start = pos;
        boolean simple = true;
        while (pos < end) {
            int b = buf.get(pos) & 0xFF;
            if (b == '"') {
                break;
            }
            if (b == '\\') {
                simple = false;
                pos++;
                if (pos < end) {
                    pos++;
                }
                continue;
            }
            pos++;
        }
        if (pos >= end) {
            throw err("unterminated string");
        }
        int contentLen = pos - start;
        pos++; // consume closing quote
        if (!simple) {
            return KEY_OTHER;
        }
        if (contentLen == 6 && regionEquals(start, "status")) {
            return KEY_STATUS;
        }
        if (contentLen == 7 && regionEquals(start, "headers")) {
            return KEY_HEADERS;
        }
        return KEY_OTHER;
    }

    /** Whether {@code buf[s .. s+lit.length())} equals the ASCII literal. */
    private boolean regionEquals(int s, String lit) {
        for (int i = 0; i < lit.length(); i++) {
            if ((buf.get(s + i) & 0xFF) != lit.charAt(i)) {
                return false;
            }
        }
        return true;
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
        // Fast path: a plain run of ASCII bytes (no escape, no byte
        // >= 0x80) — the overwhelmingly common shape for header names /
        // values — is built in one bulk copy + String construction,
        // skipping both the StringBuilder and the per-char escape / UTF-8
        // decode loop below.
        int simpleLen = simpleAsciiRun();
        if (simpleLen >= 0) {
            String s;
            if (buf.hasArray()) {
                // Heap-backed buffer (ByteBuffer.wrap on the SYNC / streaming
                // / async paths): build the String straight from the backing
                // array — one copy, no intermediate byte[].  Direct buffers
                // (the DIRECT dispatch path) have no accessible array and keep
                // the absolute bulk-get copy below.
                s =
                        new String(
                                buf.array(),
                                buf.arrayOffset() + pos,
                                simpleLen,
                                java.nio.charset.StandardCharsets.US_ASCII);
            } else {
                byte[] tmp = new byte[simpleLen];
                buf.get(pos, tmp, 0, simpleLen); // absolute bulk get (Java 13+); position untouched
                s = new String(tmp, java.nio.charset.StandardCharsets.US_ASCII);
            }
            pos += simpleLen + 1; // consume the run + the closing quote
            return s;
        }
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

    /**
     * If the string starting at {@code pos} (just past the opening quote)
     * is a plain run of ASCII bytes — no backslash escape, no byte
     * {@code >= 0x80} — terminated by a closing quote within bounds,
     * return its byte length; otherwise {@code -1}, so the caller falls
     * back to the full escape / UTF-8 decoder.  Does not move {@code pos}.
     */
    private int simpleAsciiRun() {
        int p = pos;
        while (p < end) {
            int b = buf.get(p) & 0xFF;
            if (b == '"') {
                return p - pos;
            }
            if (b == '\\' || b >= 0x80) {
                return -1;
            }
            p++;
        }
        return -1;
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
            case '"' -> skipStringRaw();
            case '{', '[' -> skipContainerRaw();
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

    /**
     * Consume a JSON string token (pos at the opening quote) without
     * allocating — the skip path never needs the decoded text, so unlike
     * {@link #readString()} it builds no {@code String}.
     */
    private void skipStringRaw() {
        pos++; // opening quote (peek() guarantees cur() == '"')
        while (pos < end) {
            int b = buf.get(pos++) & 0xFF;
            if (b == '"') {
                return;
            }
            if (b == '\\' && pos < end) {
                pos++; // skip the escaped char (so \" is not seen as the close)
            }
        }
        throw err("unterminated string");
    }

    /**
     * Consume a balanced {@code {...}} / {@code [...]} (pos at the opening
     * bracket), string-literal aware, without allocating — replaces the
     * prior recursive skip that materialised every nested key and value of
     * skipped fields ({@code metadata}, {@code validation_errors}, …).
     */
    private void skipContainerRaw() {
        int depth = 0;
        while (pos < end) {
            int b = buf.get(pos++) & 0xFF;
            switch (b) {
                case '"' -> {
                    // Skip a nested string so its braces/brackets don't count.
                    while (pos < end) {
                        int x = buf.get(pos++) & 0xFF;
                        if (x == '"') {
                            break;
                        }
                        if (x == '\\' && pos < end) {
                            pos++;
                        }
                    }
                }
                case '{', '[' -> depth++;
                case '}', ']' -> {
                    depth--;
                    if (depth == 0) {
                        return;
                    }
                }
                default -> {
                    // ordinary byte inside the container — skip
                }
            }
        }
        throw err("unterminated container");
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
