package com.devfive.vespera.bridge;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.util.Map;
import java.util.Objects;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;
import com.devfive.vespera.bridge.VesperaBridge.HeaderSink;
import com.devfive.vespera.bridge.VesperaBridge.HeaderSource;

/**
 * Binary wire-format request encoding and response decoding for
 * {@link VesperaBridge}.
 *
 * <p>This is the pure-Java, native-free half of the bridge: it turns
 * Java request parts into the length-prefixed wire bytes the Rust side
 * parses, and decodes wire responses back into a {@link DecodedResponse}.
 * It holds no JNI symbols, so it lives outside {@link VesperaBridge}
 * (whose class name is fixed by the {@code Java_com_devfive_vespera_bridge_VesperaBridge_*}
 * native symbol contract).
 *
 * <p>Wire layout (request and response share it):
 * <pre>
 *   bytes 0..4    : u32 BE = header_json byte length N
 *   bytes 4..4+N  : UTF-8 JSON header
 *   bytes 4+N..   : raw body bytes (no encoding applied)
 * </pre>
 *
 * <p>Package-private: callers go through the {@link VesperaBridge}
 * public delegators (and {@link VesperaDirectBufferPool} for the
 * direct-buffer path).
 */
final class VesperaWireCodec {

    private VesperaWireCodec() {}

    /** Lowercase hex digits for the JSON C0 control-character escapes. */
    private static final byte[] HEX = {
        '0', '1', '2', '3', '4', '5', '6', '7',
        '8', '9', 'a', 'b', 'c', 'd', 'e', 'f'
    };
    private static final int WIRE_VERSION = 1;
    /** Shared empty request body — avoids a {@code new byte[0]} per call. */
    static final byte[] EMPTY_BODY = new byte[0];
    private static final int HEADER_INITIAL_CAPACITY = 256;
    private static final int HEADER_RETAIN_CAPACITY = 32 * 1024;
    /**
     * Hard ceiling on the per-thread header encode buffer (64 MiB). The wire
     * request header only ever carries method/path/query/headers/app — never
     * the body, which is appended separately in {@link #assembleWire} /
     * {@link #assembleInto} — and servlet containers already cap inbound header
     * sizes orders of magnitude below this. It is pure defense-in-depth: a
     * pathological header that tried to grow the buffer past the ceiling fails
     * fast with an exception instead of doubling toward an OutOfMemoryError.
     */
    private static final int MAX_HEADER_BUFFER_BYTES = 64 * 1024 * 1024;

    /**
     * Per-thread reusable byte buffer for {@link #fillHeaderJson}.
     * Reset (size cleared, capacity preserved) per call and filled
     * byte-direct — no per-call encoder object.  If one request grows
     * the backing array past {@link #HEADER_RETAIN_CAPACITY}, the next
     * use on that thread drops it back to {@link #HEADER_INITIAL_CAPACITY}
     * so oversized cookies/headers do not pin a large array for the
     * servlet-thread lifetime.  Virtual-thread caveat as the direct pool:
     * each vthread gets its own ~256 B buffer in Java 21+ and loses pooling
     * until GC.
     */
    private static final ThreadLocal<ExposedByteArrayOutputStream> HEADER_BUF =
            ThreadLocal.withInitial(() -> new ExposedByteArrayOutputStream(HEADER_INITIAL_CAPACITY));

    /**
     * Drop this thread's reusable wire-header encoder buffer. Intended for
     * servlet-container shutdown/redeploy hooks; normal request handling keeps
     * the pool hot and must not call this per request.
     */
    static void clearCurrentThreadBuffers() {
        HEADER_BUF.remove();
    }

    static int currentHeaderBufferCapacityForTest() {
        return HEADER_BUF.get().capacity();
    }

    /**
     * {@link ByteArrayOutputStream} that exposes its backing array so the
     * serialized header is copied straight into the wire (heap array or
     * direct buffer) without {@link ByteArrayOutputStream#toByteArray()}
     * first materialising a second, exact-sized copy per request.
     *
     * <p>Callers MUST read only {@code [0, size())}: the backing array is
     * usually larger than the content (grow-by-doubling) and is reused
     * across calls on the same thread, so the bytes must be consumed
     * before the next {@link #fillHeaderJson} on that thread.
     */
    static final class ExposedByteArrayOutputStream extends ByteArrayOutputStream {
        ExposedByteArrayOutputStream(int size) {
            super(size);
        }

        /** Backing buffer; valid content is {@code [0, size())} only. */
        byte[] backingArray() {
            return buf;
        }

        int capacity() {
            return buf.length;
        }

        /**
         * Append one byte WITHOUT the inherited {@code synchronized} —
         * {@link #HEADER_BUF} is thread-local, so the monitor is pure
         * overhead on this single-threaded encode hot path.  Grows the
         * backing array by doubling, mirroring {@link ByteArrayOutputStream}.
         */
        void put(int b) {
            if (count == buf.length) {
                buf = java.util.Arrays.copyOf(buf, growCap(buf.length, count + 1));
            }
            buf[count++] = (byte) b;
        }

        /**
         * Append the bytes of an ASCII literal (caller guarantees every
         * char is {@code < 0x80}) — used for the fixed JSON structure
         * (keys, braces, colons).  Non-synchronized, single bulk reserve.
         */
        void putAscii(String lit) {
            int n = lit.length();
            if (count + n > buf.length) {
                buf = java.util.Arrays.copyOf(buf, growCap(buf.length, count + n));
            }
            for (int i = 0; i < n; i++) {
                buf[count++] = (byte) lit.charAt(i);
            }
        }

        /**
         * Smallest power-of-two growth of {@code current} that holds
         * {@code needed} bytes, capped at {@link #MAX_HEADER_BUFFER_BYTES}.
         * The cap is only ever consulted on a (rare) reallocation, so the
         * encode hot path pays nothing. A {@code needed} beyond the ceiling —
         * only reachable by a pathological header far larger than any servlet
         * container admits — fails fast instead of doubling toward an OOM.
         */
        private static int growCap(int current, int needed) {
            if (needed > MAX_HEADER_BUFFER_BYTES) {
                throw new IllegalArgumentException(
                        "wire header exceeds " + MAX_HEADER_BUFFER_BYTES + " bytes");
            }
            int cap = current < 1 ? 1 : current;
            while (cap < needed) {
                cap <<= 1;
                if (cap < 0 || cap > MAX_HEADER_BUFFER_BYTES) {
                    return MAX_HEADER_BUFFER_BYTES;
                }
            }
            return cap;
        }
    }

    private static final class HeaderJsonSink implements HeaderSink {
        private final ExposedByteArrayOutputStream buf;
        private boolean started;

        HeaderJsonSink(ExposedByteArrayOutputStream buf) {
            this.buf = buf;
        }

        @Override
        public void put(String lowerName, String value) {
            Objects.requireNonNull(lowerName, "header key");
            Objects.requireNonNull(value, "header value");
            if (started) {
                buf.put(',');
            } else {
                buf.putAscii(",\"headers\":{");
                started = true;
            }
            writeJsonString(buf, lowerName);
            buf.put(':');
            writeJsonString(buf, value);
        }
    }

    // ── Encode ───────────────────────────────────────────────────────

    static byte[] encodeRequest(
            String appName,
            String method,
            String path,
            String query,
            Map<String, String> headers,
        byte[] body) {
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        try {
            return assembleWire(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY);
        } finally {
            shrinkHeaderBufferIfOversized(hdr);
        }
    }

    static byte[] encodeRequest(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
        byte[] body) {
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        try {
            return assembleWire(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY);
        } finally {
            shrinkHeaderBufferIfOversized(hdr);
        }
    }

    static int encodeRequestInto(
            String appName,
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body,
        ByteBuffer target) {
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        try {
            return assembleInto(
                    hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY, target);
        } finally {
            shrinkHeaderBufferIfOversized(hdr);
        }
    }

    static int encodeRequestInto(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body,
        ByteBuffer target) {
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        try {
            return assembleInto(
                    hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY, target);
        } finally {
            shrinkHeaderBufferIfOversized(hdr);
        }
    }

    /**
     * Total wire length {@code 4 + headerLen + bodyLen}, computed in
     * {@code long} and validated against {@code Integer.MAX_VALUE}.
     *
     * <p>A body approaching the ~2 GiB Java array limit would otherwise
     * overflow the {@code int} addition into a negative / small value,
     * corrupting capacity checks ({@code target.capacity() < total}) and
     * array sizing ({@code new byte[...]} → {@code NegativeArraySizeException}).
     * A buffered wire request cannot exceed 2 GiB on the JVM regardless, so
     * an overflow is a hard, explanatory {@link IllegalArgumentException}
     * pointing the caller at streaming dispatch — never a silent corruption.
     */
    static int wireTotalLength(int headerLen, int bodyLen) {
        long total = 4L + headerLen + bodyLen;
        if (total > Integer.MAX_VALUE) {
            throw new IllegalArgumentException(
                    "wire request exceeds 2 GiB (4 + headerLen=" + headerLen
                            + " + bodyLen=" + bodyLen + " = " + total
                            + " bytes); use streaming dispatch for payloads this large");
        }
        return (int) total;
    }

    /**
     * Decode and validate the u32 BE header-length prefix at bytes {@code 0..4}
     * of a heap wire frame — the <strong>single source of truth</strong> for
     * the frame split shared by {@link #decodeResponse} and the
     * {@link VesperaProxyController} write/build paths, so the bounds contract
     * can never drift between the (previously duplicated) call sites.
     *
     * <p>The prefix is read from absolute bytes (big-endian, order-independent),
     * never {@code ByteBuffer.getInt} which honours the buffer's current byte
     * order.
     *
     * @return the header JSON length {@code N} (so the body is {@code wire[4+N..]})
     * @throws IllegalArgumentException if {@code wire} is shorter than the
     *     4-byte prefix, or the decoded length is negative or overflows the frame
     */
    static int readHeaderLength(byte[] wire) {
        if (wire == null || wire.length < 4) {
            throw new IllegalArgumentException(
                    "wire response too short: "
                            + (wire == null ? "null" : wire.length + " bytes"));
        }
        int headerLen = ((wire[0] & 0xFF) << 24) | ((wire[1] & 0xFF) << 16)
                | ((wire[2] & 0xFF) << 8) | (wire[3] & 0xFF);
        if (headerLen < 0 || 4L + headerLen > wire.length) {
            throw new IllegalArgumentException(
                    "wire header_len " + headerLen
                            + " overflows response (" + wire.length + " bytes)");
        }
        return headerLen;
    }

    /**
     * {@link ByteBuffer} sibling of {@link #readHeaderLength(byte[])} — decodes
     * the u32 BE header-length prefix from absolute bytes {@code 0..4} of
     * {@code wire} (honouring neither the buffer's position nor its byte order),
     * validating against {@code wire.limit()}.
     *
     * @return the header JSON length {@code N}
     * @throws IllegalArgumentException if the buffer is shorter than the 4-byte
     *     prefix, or the decoded length is negative or overflows the limit
     */
    static int readHeaderLength(ByteBuffer wire) {
        int limit = wire.limit();
        if (limit < 4) {
            throw new IllegalArgumentException("wire response too short: " + limit + " bytes");
        }
        int headerLen = ((wire.get(0) & 0xFF) << 24)
                | ((wire.get(1) & 0xFF) << 16)
                | ((wire.get(2) & 0xFF) << 8)
                | (wire.get(3) & 0xFF);
        if (headerLen < 0 || 4L + headerLen > limit) {
            throw new IllegalArgumentException(
                    "wire header_len " + headerLen + " overflows response (" + limit + " bytes)");
        }
        return headerLen;
    }

    /** Internal: write {@code [u32 BE len | headerJson[0..headerLen] | body]} at position 0. */
    static int assembleInto(byte[] headerJson, int headerLen, byte[] body, ByteBuffer target) {
        int total = wireTotalLength(headerLen, body.length);
        if (target.capacity() < total) {
            return -total;
        }
        if (target.isReadOnly()) {
            throw new IllegalArgumentException("encode target buffer is read-only");
        }
        target.clear();
        target.put((byte) (headerLen >>> 24));
        target.put((byte) (headerLen >>> 16));
        target.put((byte) (headerLen >>> 8));
        target.put((byte) headerLen);
        target.put(headerJson, 0, headerLen);
        if (body.length > 0) {
            target.put(body);
        }
        return total;
    }

    /** Internal: assemble a heap wire array from pre-serialised parts. */
    static byte[] assembleWire(byte[] headerJson, int headerLen, byte[] body) {
        byte[] wire = new byte[wireTotalLength(headerLen, body.length)];
        // Write the u32 BE length prefix directly — avoids the
        // HeapByteBuffer wrapper object that
        // ByteBuffer.allocate(...).array() allocates per request; the
        // arraycopy intrinsics handle the header + body.  Byte-identical
        // to the prior ByteBuffer path.
        wire[0] = (byte) (headerLen >>> 24);
        wire[1] = (byte) (headerLen >>> 16);
        wire[2] = (byte) (headerLen >>> 8);
        wire[3] = (byte) headerLen;
        System.arraycopy(headerJson, 0, wire, 4, headerLen);
        System.arraycopy(body, 0, wire, 4 + headerLen, body.length);
        return wire;
    }

    /**
     * Internal: serialise the wire request header JSON
     * <strong>byte-direct</strong> into the per-thread {@link #HEADER_BUF}
     * — no Jackson generator (and its per-call object + scratch buffer)
     * is allocated.  Emits the same shape and field order the prior
     * {@code JsonGenerator} path did ({@code v}, {@code method},
     * {@code path}, optional {@code query}/{@code headers}/{@code app}),
     * with the same omission rules.  String values are escaped + UTF-8
     * encoded by {@link #writeJsonString} using exactly the escape set
     * Jackson's {@code UTF8JsonGenerator} produced (the quote, the
     * backslash, and the C0 controls; {@code /} and non-ASCII pass
     * through), so the bytes stay valid JSON the Rust {@code serde_json}
     * side parses identically.
     */
    static ExposedByteArrayOutputStream fillHeaderJson(String appName, String method,
            String path, String query, Map<String, String> headers) {
        String normalizedAppName = normalizedAppName(appName);
        ExposedByteArrayOutputStream buf = reusableHeaderBuffer();
        try {
            Objects.requireNonNull(method, "method");
            Objects.requireNonNull(path, "path");
            buf.putAscii("{\"v\":");
            // WIRE_VERSION is a single-digit constant; write its ASCII digit
            // directly to avoid the per-request `Integer.toString(1)` allocation
            // the old `writeAsciiInt` made on every encode. Byte-identical output.
            buf.put((byte) ('0' + WIRE_VERSION));
            buf.putAscii(",\"method\":");
            writeJsonString(buf, method);
            buf.putAscii(",\"path\":");
            writeCombinedPath(buf, path, query);
            if (headers != null && !headers.isEmpty()) {
                buf.putAscii(",\"headers\":{");
                boolean first = true;
                for (Map.Entry<String, String> e : headers.entrySet()) {
                    if (!first) {
                        buf.put(',');
                    }
                    first = false;
                    writeJsonString(buf, Objects.requireNonNull(e.getKey(), "header key"));
                    buf.put(':');
                    writeJsonString(buf, Objects.requireNonNull(e.getValue(), "header value"));
                }
                buf.put('}');
            }
            if (normalizedAppName != null) {
                buf.putAscii(",\"app\":");
                writeJsonString(buf, normalizedAppName);
            }
            buf.put('}');
            return buf;
        } catch (RuntimeException | Error failure) {
            shrinkHeaderBufferIfOversized(buf);
            throw failure;
        }
    }

    static ExposedByteArrayOutputStream fillHeaderJson(String appName, String method,
            String path, String query, HeaderSource headers) {
        String normalizedAppName = normalizedAppName(appName);
        ExposedByteArrayOutputStream buf = reusableHeaderBuffer();
        try {
            Objects.requireNonNull(method, "method");
            Objects.requireNonNull(path, "path");
            buf.putAscii("{\"v\":");
            // WIRE_VERSION is a single-digit constant; write its ASCII digit
            // directly to avoid the per-request `Integer.toString(1)` allocation
            // the old `writeAsciiInt` made on every encode. Byte-identical output.
            buf.put((byte) ('0' + WIRE_VERSION));
            buf.putAscii(",\"method\":");
            writeJsonString(buf, method);
            buf.putAscii(",\"path\":");
            writeCombinedPath(buf, path, query);
            if (headers != null) {
                HeaderJsonSink sink = new HeaderJsonSink(buf);
                headers.writeTo(sink);
                if (sink.started) {
                    buf.put('}');
                }
            }
            if (normalizedAppName != null) {
                buf.putAscii(",\"app\":");
                writeJsonString(buf, normalizedAppName);
            }
            buf.put('}');
            return buf;
        } catch (RuntimeException | Error failure) {
            shrinkHeaderBufferIfOversized(buf);
            throw failure;
        }
    }

    static String normalizedAppName(String appName) {
        if (appName == null) {
            return null;
        }
        int start = 0;
        int end = appName.length();
        while (start < end && Character.isWhitespace(appName.charAt(start))) {
            start++;
        }
        while (end > start && Character.isWhitespace(appName.charAt(end - 1))) {
            end--;
        }
        if (start == end) {
            return null;
        }
        return start == 0 && end == appName.length() ? appName : appName.substring(start, end);
    }

    private static ExposedByteArrayOutputStream reusableHeaderBuffer() {
        ExposedByteArrayOutputStream buf = HEADER_BUF.get();
        if (buf.capacity() > HEADER_RETAIN_CAPACITY) {
            buf = new ExposedByteArrayOutputStream(HEADER_INITIAL_CAPACITY);
            HEADER_BUF.set(buf);
        } else {
            buf.reset();
        }
        return buf;
    }

    /**
     * Proactively release a per-thread header buffer that one pathologically
     * large header grew past {@link #HEADER_RETAIN_CAPACITY}. Called right
     * after the header is built and consumed, so the oversized backing array
     * is dropped immediately instead of staying pinned for the servlet
     * thread's lifetime until that thread happens to encode another request.
     *
     * <p>The bytes have already been consumed by the caller
     * ({@code assembleWire} / {@code assembleInto} / {@code dispatchBytes})
     * before this runs, so replacing the buffer here is safe.
     * {@link #reusableHeaderBuffer()} still keeps its lazy shrink as a
     * defense-in-depth fallback for any path that does not call this.
     */
    static void shrinkHeaderBufferIfOversized(ExposedByteArrayOutputStream buf) {
        if (buf.capacity() > HEADER_RETAIN_CAPACITY) {
            HEADER_BUF.set(new ExposedByteArrayOutputStream(HEADER_INITIAL_CAPACITY));
        }
    }

    /**
     * Append {@code s} as a quoted JSON string straight into {@code out}
     * as UTF-8, escaping only the JSON-mandatory characters — the quote,
     * the backslash, and the C0 controls (short {@code \b \t \n \f \r}
     * forms, four-hex escapes otherwise) — exactly the set the prior
     * Jackson {@code UTF8JsonGenerator} emitted (it does not escape
     * {@code /} or non-ASCII).  Single pass, no per-string {@code byte[]}:
     * printable ASCII is written verbatim, the rest UTF-8 encoded inline
     * (surrogate pairs become 4-byte sequences).
     */
    private static void writeJsonString(ExposedByteArrayOutputStream out, String s) {
        out.put('"');
        writeJsonStringBody(out, s);
        out.put('"');
    }

    /**
     * Write the {@code "path"} field VALUE as the full request target.  When a
     * query is present, emit {@code "path?query"} as ONE JSON string
     * (byte-direct — no intermediate Java {@code String} concat); otherwise the
     * escaped path alone.  Folding the query into {@code path} drops the
     * separate {@code query} wire field so the Rust dispatch side borrows the
     * target for {@code Uri} parsing instead of re-joining {@code path + '?' +
     * query}.  Byte-equivalent to the prior two-field form after URI parsing
     * (axum routes on the path component, the query is preserved verbatim).
     */
    private static void writeCombinedPath(
            ExposedByteArrayOutputStream out, String path, String query) {
        if (query == null || query.isEmpty()) {
            writeJsonString(out, path);
            return;
        }
        out.put('"');
        writeJsonStringBody(out, path);
        out.put('?');
        writeJsonStringBody(out, query);
        out.put('"');
    }

    /**
     * Write the escaped UTF-8 <strong>body</strong> of a JSON string — the same
     * bytes {@link #writeJsonString} emits but WITHOUT the surrounding quotes —
     * so a caller can concatenate several escaped segments inside ONE JSON
     * string.  Used to emit the request target {@code path?query} as a single
     * {@code "path"} field (no separate {@code query} field), so the Rust
     * dispatch side borrows the target directly instead of re-joining
     * {@code path + '?' + query} (~4% per query-GET; see the Rust `query_path`
     * bench and {@code wire_contract.rs}).
     */
    private static void writeJsonStringBody(ExposedByteArrayOutputStream out, String s) {
        int n = s.length();
        for (int i = 0; i < n; i++) {
            char c = s.charAt(i);
            if (c >= 0x20 && c < 0x80) {
                if (c == '"' || c == '\\') {
                    out.put('\\');
                }
                out.put(c);
            } else if (c < 0x20) {
                switch (c) {
                    case '\b' -> {
                        out.put('\\');
                        out.put('b');
                    }
                    case '\t' -> {
                        out.put('\\');
                        out.put('t');
                    }
                    case '\n' -> {
                        out.put('\\');
                        out.put('n');
                    }
                    case '\f' -> {
                        out.put('\\');
                        out.put('f');
                    }
                    case '\r' -> {
                        out.put('\\');
                        out.put('r');
                    }
                    default -> {
                        out.put('\\');
                        out.put('u');
                        out.put('0');
                        out.put('0');
                        out.put(HEX[(c >> 4) & 0xF]);
                        out.put(HEX[c & 0xF]);
                    }
                }
            } else if (c < 0x800) {
                out.put(0xC0 | (c >> 6));
                out.put(0x80 | (c & 0x3F));
            } else if (Character.isHighSurrogate(c)
                    && i + 1 < n
                    && Character.isLowSurrogate(s.charAt(i + 1))) {
                int cp = Character.toCodePoint(c, s.charAt(++i));
                out.put(0xF0 | (cp >> 18));
                out.put(0x80 | ((cp >> 12) & 0x3F));
                out.put(0x80 | ((cp >> 6) & 0x3F));
                out.put(0x80 | (cp & 0x3F));
            } else if (Character.isSurrogate(c)) {
                // Unpaired UTF-16 surrogate (a lone high surrogate not
                // followed by a low surrogate, or a lone low surrogate).
                // UTF-8 must never encode surrogate code points, so emit a
                // six-character JSON escape (backslash, u, four hex digits)
                // instead of the invalid 3-byte sequence the BMP branch
                // below would produce — this keeps the wire header valid
                // UTF-8 / RFC 8259 JSON and round-trips losslessly through
                // serde_json on the Rust side.
                out.put('\\');
                out.put('u');
                out.put(HEX[(c >> 12) & 0xF]);
                out.put(HEX[(c >> 8) & 0xF]);
                out.put(HEX[(c >> 4) & 0xF]);
                out.put(HEX[c & 0xF]);
            } else {
                out.put(0xE0 | (c >> 12));
                out.put(0x80 | ((c >> 6) & 0x3F));
                out.put(0x80 | (c & 0x3F));
            }
        }
    }

    // ── Decode ─────────────────────────────────────────────────────────

    /**
     * Decode a wire-format response.
     *
     * @throws IllegalArgumentException if the wire bytes are malformed
     */
    static DecodedResponse decodeResponse(byte[] wire) {
        int headerLen = readHeaderLength(wire);
        // Manual decode via the allocation-lean WireHeaderReader tokenizer
        // (the same parser the DIRECT / streaming header callbacks use)
        // instead of a Jackson JsonParser — drops the per-response parser +
        // IOContext allocation.  Output is shape-identical: status (default
        // 500), headers (String | List<String>), metadata (pre-sized),
        // validation_errors, and unknown fields (incl. "v") skipped.
        WireHeaderReader.Decoded d = WireHeaderReader.decode(wire, 4, headerLen);
        ByteBuffer buf = ByteBuffer.wrap(wire);
        buf.position(4 + headerLen).limit(wire.length);
        ByteBuffer body = buf.slice().asReadOnlyBuffer();
        return new DecodedResponse(
                d.status,
                d.headers == null ? Map.of() : d.headers,
                d.metadata,
                body,
                d.validationErrors);
    }
}
