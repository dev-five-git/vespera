package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
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

    /**
     * Drop this thread's direct-buffer string decode scratch. Intended for
     * servlet-container shutdown/redeploy cleanup; do not call per request.
     */
    static void clearCurrentThreadBuffers() {
        WireHeaderStringSupport.clearCurrentThreadBuffers();
    }

    private final ByteBuffer buf;
    private final byte[] array;
    private int pos;
    private final int end;

    private WireHeaderReader(ByteBuffer buf, int off, int len) {
        this.buf = buf;
        this.array = null;
        this.pos = off;
        this.end = off + len;
    }

    private WireHeaderReader(byte[] array, int off, int len) {
        this.buf = null;
        this.array = array;
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
        applyInner(new WireHeaderReader(buf, off, len), statusSink, headerSink);
    }

    static void apply(
            byte[] buf,
            int off,
            int len,
            IntConsumer statusSink,
            BiConsumer<String, String> headerSink) {
        applyInner(new WireHeaderReader(buf, off, len), statusSink, headerSink);
    }

    /**
     * Shared tokenizer body for both {@link #apply(ByteBuffer, int, int,
     * IntConsumer, BiConsumer)} and {@link #apply(byte[], int, int,
     * IntConsumer, BiConsumer)} — they differ only in which constructor
     * built the reader, and the reader's {@link #byteAt} already branches
     * on whichever backing storage is non-null, so the parse loop is
     * byte-identical between the two overloads.
     */
    private static void applyInner(
            WireHeaderReader r,
            IntConsumer statusSink,
            BiConsumer<String, String> headerSink) {
        int status = 500;
        r.requireObjectStart();
        r.beginObject();
        int seen = 0;
        int key;
        while ((key = r.nextRootKey()) != KEY_END) {
            seen = r.rejectDuplicateRootKey(seen, key);
            switch (key) {
                case KEY_STATUS -> status = r.readStatusCode();
                case KEY_HEADERS -> {
                    if (r.isObjectStart()) {
                        r.beginObject();
                        String k;
                        // Canonical keys reuse one shared String per common
                        // header name (content-type, content-length, …) —
                        // the same allocation-free path decode() uses, so
                        // the per-request DIRECT/streaming apply() no longer
                        // allocates a fresh key String for each header.
                        while ((k = r.nextKeyCanonical()) != null) {
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
        r.requireFullyConsumed();
        statusSink.accept(status);
    }

    /** Decoded response-header components (see {@link #decode}). */
    static final class Decoded {
        int status = 500;
        Map<String, Object> headers;
        // Defaults to the shared empty immutable map; overwritten by decode()
        // when a metadata object is present — a single-entry Map.of for the
        // common {"version":...} shape (no hash table), a LinkedHashMap only
        // for the rare 2+ key case.
        Map<String, String> metadata = Map.of();
        List<Map<String, Object>> validationErrors;
    }

    /**
     * Full decode of the response wire header for
     * {@link VesperaBridge#decodeResponse(byte[])} — {@code status},
     * {@code headers} ({@link String} or {@link List}&lt;String&gt; for
     * multi-valued names), {@code metadata}, and {@code validation_errors}
     * — reusing this reader's tested tokenizer instead of allocating a
     * Jackson {@code JsonParser} + {@code IOContext} per response.
     *
     * <p>Output is shape-identical to the prior Jackson path for the
     * well-formed, fixed-schema header the Rust {@code serde_json} side
     * emits: status defaults to {@code 500} when absent; {@code headers}
     * stays {@code null} when no header field is present; {@code metadata}
     * is always a (possibly empty) map; {@code validationErrors} is
     * {@code null} unless the {@code validation_errors} field is present;
     * unknown fields (incl. {@code v}) are skipped without materialising.
     */
    static Decoded decode(ByteBuffer buf, int off, int len) {
        WireHeaderReader r = new WireHeaderReader(buf, off, len);
        return r.decodeRoot();
    }

    static Decoded decode(byte[] buf, int off, int len) {
        WireHeaderReader r = new WireHeaderReader(buf, off, len);
        return r.decodeRoot();
    }

    private Decoded decodeRoot() {
        Decoded out = new Decoded();
        requireObjectStart();
        beginObject();
        int seen = 0;
        int key;
        while ((key = nextRootKey()) != KEY_END) {
            seen = rejectDuplicateRootKey(seen, key);
            switch (key) {
                case KEY_STATUS -> out.status = readStatusCode();
                case KEY_HEADERS -> {
                    if (isObjectStart()) {
                        beginObject();
                        String k;
                        while ((k = nextKeyCanonical()) != null) {
                            if (out.headers == null) {
                                // Pre-size for a typical response header
                                // count (content-type, content-length, …).
                                out.headers = new LinkedHashMap<>(8);
                            }
                            if (isArrayStart()) {
                                beginArray();
                                List<String> list = new ArrayList<>();
                                while (hasNextElement()) {
                                    list.add(readString());
                                }
                                out.headers.put(k, list);
                            } else {
                                out.headers.put(k, readString());
                            }
                        }
                    } else {
                        skipValue();
                    }
                }
                case KEY_METADATA -> {
                    if (isObjectStart()) {
                        beginObject();
                        out.metadata = readStringMap();
                    } else {
                        skipValue();
                    }
                }
                case KEY_VALIDATION -> {
                    if (isArrayStart()) {
                        beginArray();
                        out.validationErrors = new ArrayList<>();
                        while (hasNextElement()) {
                            if (!isObjectStart()) {
                                // Fixed schema is an array of objects; a
                                // non-object element (only on malformed
                                // input) is skipped so the cursor still
                                // reaches the array end cleanly.
                                skipValue();
                                continue;
                            }
                            beginObject();
                            Map<String, Object> entry = new LinkedHashMap<>(4);
                            String k;
                            while ((k = nextKeyCanonical()) != null) {
                                entry.put(k, readPrimitiveValue());
                            }
                            out.validationErrors.add(entry);
                        }
                    } else {
                        skipValue();
                    }
                }
                // KEY_OTHER: "v" and any unknown field — value skipped,
                // never materialised.
                default -> skipValue();
            }
        }
        requireFullyConsumed();
        return out;
    }

    /**
     * Read a string→string object (the {@code metadata} shape) into the
     * smallest map: {@link Map#of()} when empty, a single-entry immutable
     * {@link Map#of(Object, Object)} for the overwhelmingly common one-key
     * case ({@code {"version":...}}) — no hash table allocated — and a
     * mutable {@link LinkedHashMap} only for the rare 2+ key case (which
     * also tolerates duplicate keys, last-wins, like the prior map).
     * Assumes the object was already entered ({@link #beginObject}).
     */
    Map<String, String> readStringMap() {
        String k0 = nextKeyCanonical();
        if (k0 == null) {
            return Map.of();
        }
        String v0 = readString();
        String k1 = nextKeyCanonical();
        if (k1 == null) {
            return Map.of(k0, v0);
        }
        Map<String, String> m = new LinkedHashMap<>(8);
        m.put(k0, v0);
        m.put(k1, readString());
        String k;
        while ((k = nextKeyCanonical()) != null) {
            m.put(k, readString());
        }
        return m;
    }

    private void skipWs() {
        while (pos < end) {
            int c = byteAt(pos);
            if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                pos++;
            } else {
                break;
            }
        }
    }

    private int cur() {
        return pos < end ? byteAt(pos) : -1;
    }

    private int byteAt(int index) {
        return array != null ? array[index] & 0xFF : buf.get(index) & 0xFF;
    }

    private void requireFullyConsumed() {
        skipWs();
        if (pos != end) {
            throw err("trailing data after root object");
        }
    }

    int peek() {
        skipWs();
        return cur();
    }

    private IllegalArgumentException err(String what) {
        return new IllegalArgumentException("wire header JSON: " + what + " at offset " + pos);
    }

    private void requireObjectStart() {
        if (peek() != '{') {
            throw err("expected object");
        }
    }

    private int rejectDuplicateRootKey(int seen, int key) {
        if (key < 0) {
            return seen;
        }
        int bit = 1 << key;
        if ((seen & bit) != 0) {
            throw err("duplicate root key");
        }
        return seen | bit;
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

    /**
     * Well-known response wire keys, kept as shared (interned string-literal)
     * instances so the per-response header / metadata / validation maps reuse
     * one canonical key String instead of allocating a fresh one each call —
     * the allocation Jackson's symbol table used to elide.  Plain ASCII by
     * construction (HTTP field names + the fixed metadata / validation keys).
     */
    /**
     * If the upcoming quoted member key is a plain-ASCII canonical-key entry,
     * consume it (key + closing quote) and return the shared instance;
     * otherwise leave {@code pos} untouched and return {@code null} so the
     * caller falls back to {@link #readString()} — escaped / non-ASCII /
     * unknown keys still allocate exactly as before.
     */
    private String peekCanonicalKey() {
        if (cur() != '"') {
            return null;
        }
        int p = pos + 1;
        int start = p;
        while (p < end) {
            int b = byteAt(p);
            if (b == '"') {
                break;
            }
            if (b == '\\' || b >= 0x80) {
                return null;
            }
            p++;
        }
        if (p >= end) {
            return null;
        }
        String canon = canonicalKey(start, p - start);
        if (canon != null) {
            pos = p + 1;
            return canon;
        }
        return null;
    }

    /**
     * {@link #nextKey()} that returns a shared canonical key for the common
     * wire keys (allocation-free) and falls back to {@link #readString()} for
     * the rest — used by {@link #decode} for the header / metadata /
     * validation member keys.
     */
    String nextKeyCanonical() {
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
        String canon = peekCanonicalKey();
        String key = (canon != null) ? canon : readString();
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
    // Recognised additionally by the full decode() path (apply() skips these
    // as KEY_OTHER); matched allocation-free by length + bytes like the rest.
    private static final int KEY_METADATA = 2;
    private static final int KEY_VALIDATION = 3;

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
            int b = byteAt(pos);
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
        if (contentLen == 8 && regionEquals(start, "metadata")) {
            return KEY_METADATA;
        }
        if (contentLen == 17 && regionEquals(start, "validation_errors")) {
            return KEY_VALIDATION;
        }
        return KEY_OTHER;
    }

    private boolean regionEquals(int s, String lit) {
        return array != null
                ? WireHeaderStringSupport.regionEquals(array, s, lit)
                : WireHeaderStringSupport.regionEquals(buf, s, lit);
    }

    private String canonicalKey(int start, int len) {
        return array != null
                ? WireHeaderStringSupport.canonicalKey(array, start, len)
                : WireHeaderStringSupport.canonicalKey(buf, start, len);
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
            // `readAsciiString` already branches on `buf.hasArray()` itself:
            // heap-backed buffers (SYNC / streaming / async, ByteBuffer.wrap)
            // build the String straight from the backing array (one copy, no
            // intermediate byte[]), while direct buffers (the DIRECT dispatch
            // path) fall back to a pooled-scratch bulk-get — so this single
            // call is already optimal for both buffer kinds. The previous outer
            // `if (buf.hasArray()) ... else ...` invoked the identical call in
            // both arms (dead branch); collapsed here.
            String s = readAsciiString(pos, simpleLen);
            pos += simpleLen + 1; // consume the run + the closing quote
            return s;
        }
        StringBuilder sb = new StringBuilder(Math.min(end - pos, 256));
        while (pos < end) {
            int b = byteAt(pos++);
            if (b == '"') {
                return sb.toString();
            }
            if (b == '\\') {
                if (pos >= end) {
                    throw err("dangling escape");
                }
                int e = byteAt(pos++);
                switch (e) {
                    case '"' -> sb.append('"');
                    case '\\' -> sb.append('\\');
                    case '/' -> sb.append('/');
                    case 'b' -> sb.append('\b');
                    case 'f' -> sb.append('\f');
                    case 'n' -> sb.append('\n');
                    case 'r' -> sb.append('\r');
                    case 't' -> sb.append('\t');
                    case 'u' -> appendUnicodeEscape(sb);
                    default -> throw err("bad escape");
                }
            } else if (b < 0x80) {
                sb.append((char) b);
            } else if (b < 0xE0) {
                if (b < 0xC2) {
                    throw err("bad UTF-8");
                }
                sb.append((char) (((b & 0x1F) << 6) | nextCont()));
            } else if (b < 0xF0) {
                int c1 = nextContByte();
                if ((b == 0xE0 && c1 < 0xA0) || (b == 0xED && c1 >= 0xA0)) {
                    throw err("bad UTF-8");
                }
                sb.append((char) (((b & 0x0F) << 12) | ((c1 & 0x3F) << 6) | nextCont()));
            } else if (b < 0xF5) {
                int c1 = nextContByte();
                if ((b == 0xF0 && c1 < 0x90) || (b == 0xF4 && c1 > 0x8F)) {
                    throw err("bad UTF-8");
                }
                int cp = ((b & 0x07) << 18) | ((c1 & 0x3F) << 12) | (nextCont() << 6) | nextCont();
                sb.appendCodePoint(cp);
            } else {
                throw err("bad UTF-8");
            }
        }
        throw err("unterminated string");
    }

    /**
     * Read the primitive JSON values allowed inside validation error maps.
     * Strings keep the established shape; numbers, booleans, and null are
     * accepted so future Rust-side hoisted fields do not make Java decoding
     * fail. Containers are still outside this fixed schema and are skipped.
     */
    Object readPrimitiveValue() {
        int c = peek();
        return switch (c) {
            case '"' -> readString();
            case 't' -> {
                consumeLiteral("true");
                yield Boolean.TRUE;
            }
            case 'f' -> {
                consumeLiteral("false");
                yield Boolean.FALSE;
            }
            case 'n' -> {
                consumeLiteral("null");
                yield null;
            }
            case '{', '[' -> {
                skipContainerRaw();
                yield null;
            }
            default -> {
                if (c == '-' || (c >= '0' && c <= '9')) {
                    yield readNumberValue();
                }
                throw err("unexpected primitive value");
            }
        };
    }

    private Object readNumberValue() {
        skipWs();
        int start = pos;
        if (cur() == '-') {
            pos++;
        }
        boolean anyDigit = readDigits();
        boolean floating = false;
        if (cur() == '.') {
            floating = true;
            pos++;
            if (!readDigits()) {
                throw err("expected digit after decimal point");
            }
        }
        int c = cur();
        if (c == 'e' || c == 'E') {
            floating = true;
            pos++;
            c = cur();
            if (c == '+' || c == '-') {
                pos++;
            }
            if (!readDigits()) {
                throw err("expected digit in exponent");
            }
        }
        if (!anyDigit) {
            pos = start;
            throw err("expected number");
        }
        String token = asciiToken(start, pos - start);
        try {
            if (floating) {
                return Double.valueOf(token);
            }
            return Long.valueOf(token);
        } catch (NumberFormatException overflowOrNan) {
            return Double.valueOf(token);
        }
    }

    private boolean readDigits() {
        boolean any = false;
        while (pos < end) {
            int d = byteAt(pos);
            if (d < '0' || d > '9') {
                break;
            }
            pos++;
            any = true;
        }
        return any;
    }

    private String asciiToken(int start, int len) {
        return readAsciiString(start, len);
    }

    private String readAsciiString(int start, int len) {
        return array != null
                ? WireHeaderStringSupport.readAsciiString(array, start, len)
                : WireHeaderStringSupport.readAsciiString(buf, start, len);
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
            int b = byteAt(p);
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
        return nextContByte() & 0x3F;
    }

    private int nextContByte() {
        if (pos >= end) {
            throw err("truncated UTF-8");
        }
        int b = byteAt(pos++);
        if ((b & 0xC0) != 0x80) {
            throw err("bad UTF-8 continuation");
        }
        return b;
    }

    private char readHex4() {
        if (pos + 4 > end) {
            throw err("truncated unicode escape");
        }
        int v = 0;
        for (int k = 0; k < 4; k++) {
            int d = byteAt(pos++);
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

    private void appendUnicodeEscape(StringBuilder sb) {
        char c = readHex4();
        if (Character.isHighSurrogate(c)) {
            if (pos + 6 > end || byteAt(pos) != '\\' || byteAt(pos + 1) != 'u') {
                throw err("unpaired unicode surrogate");
            }
            pos += 2;
            char low = readHex4();
            if (!Character.isLowSurrogate(low)) {
                throw err("unpaired unicode surrogate");
            }
            sb.appendCodePoint(Character.toCodePoint(c, low));
            return;
        }
        if (Character.isLowSurrogate(c)) {
            throw err("unpaired unicode surrogate");
        }
        sb.append(c);
    }

    int readStatusCode() {
        skipWs();
        int start = pos;
        boolean neg = cur() == '-';
        if (neg) {
            pos++;
        }
        boolean any = false;
        long v = 0;
        long limit = neg ? 2147483648L : Integer.MAX_VALUE;
        while (pos < end) {
            int d = byteAt(pos);
            if (d < '0' || d > '9') {
                break;
            }
            v = v * 10 + (d - '0');
            if (v > limit) {
                throw err("integer overflow");
            }
            pos++;
            any = true;
        }
        if (pos < end) {
            int c = cur();
            if (c == '.' || c == 'e' || c == 'E') {
                // `status` is a protocol INTEGER field; a fraction/exponent
                // (e.g. `200.9`, `2e2`) is malformed native output, NOT a
                // value to silently truncate to its integer part.  Unknown
                // numeric fields stay permissive via `skipNumberRaw`.
                throw err("status must be an integer (no fraction or exponent)");
            }
        }
        if (!any) {
            pos = start;
            throw err("expected number");
        }
        int status = (int) (neg ? -v : v);
        if (status < 100 || status > 999) {
            throw err("status out of range");
        }
        return status;
    }

    private void skipNumberTail() {
        while (pos < end) {
            int d = byteAt(pos);
            if ((d >= '0' && d <= '9') || d == '.' || d == 'e' || d == 'E' || d == '+' || d == '-') {
                pos++;
            } else {
                break;
            }
        }
    }

    /**
     * Consume a JSON number token (sign, integer digits, optional fraction
     * and exponent) WITHOUT parsing it to an {@code int}.  The skip path
     * discards unknown-field values, so an unknown numeric that is large
     * (beyond {@code int} range) or a decimal must NOT fail decode the way
     * {@link #readStatusCode} — used for the known, overflow-checked {@code status}
     * field — would.  Forward-compatibility for newer / custom wire headers.
     */
    private void skipNumberRaw() {
        skipWs();
        if (cur() == '-') {
            pos++;
        }
        int digitsStart = pos;
        skipNumberTail();
        if (pos == digitsStart) {
            throw err("expected number");
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
                    skipNumberRaw();
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
            int b = byteAt(pos++);
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
            int b = byteAt(pos++);
            switch (b) {
                case '"' -> {
                    // Skip a nested string so its braces/brackets don't count.
                    while (pos < end) {
                        int x = byteAt(pos++);
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
        int c = cur();
        if (c == 't') {
            consumeLiteral("true");
        } else if (c == 'f') {
            consumeLiteral("false");
        } else if (c == 'n') {
            consumeLiteral("null");
        } else {
            throw err("expected literal");
        }
    }

    private void consumeLiteral(String literal) {
        for (int i = 0; i < literal.length(); i++) {
            if (pos + i >= end || byteAt(pos + i) != literal.charAt(i)) {
                throw err("expected " + literal);
            }
        }
        pos += literal.length();
    }
}
