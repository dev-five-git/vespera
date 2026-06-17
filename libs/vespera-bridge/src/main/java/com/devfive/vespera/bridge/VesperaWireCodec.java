package com.devfive.vespera.bridge;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.Map;

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
        return assembleWire(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY);
    }

    static byte[] encodeRequest(
            String appName,
            String method,
            String path,
            String query,
            HeaderSource headers,
            byte[] body) {
        ExposedByteArrayOutputStream hdr = fillHeaderJson(appName, method, path, query, headers);
        return assembleWire(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY);
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
        return assembleInto(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY, target);
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
        return assembleInto(hdr.backingArray(), hdr.size(), body != null ? body : EMPTY_BODY, target);
    }

    /** Internal: write {@code [u32 BE len | headerJson[0..headerLen] | body]} at position 0. */
    static int assembleInto(byte[] headerJson, int headerLen, byte[] body, ByteBuffer target) {
        int total = 4 + headerLen + body.length;
        if (target.capacity() < total) {
            return -total;
        }
        target.clear();
        target.order(ByteOrder.BIG_ENDIAN);
        target.putInt(headerLen);
        target.put(headerJson, 0, headerLen);
        if (body.length > 0) {
            target.put(body);
        }
        return total;
    }

    /** Internal: assemble a heap wire array from pre-serialised parts. */
    static byte[] assembleWire(byte[] headerJson, int headerLen, byte[] body) {
        byte[] wire = new byte[4 + headerLen + body.length];
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
        ExposedByteArrayOutputStream buf = reusableHeaderBuffer();
        // {"v":<WIRE_VERSION>, ...} — WIRE_VERSION is a single decimal digit.
        buf.putAscii("{\"v\":");
        buf.put('0' + WIRE_VERSION);
        buf.putAscii(",\"method\":");
        writeJsonString(buf, method);
        buf.putAscii(",\"path\":");
        writeJsonString(buf, path);
        if (query != null && !query.isEmpty()) {
            buf.putAscii(",\"query\":");
            writeJsonString(buf, query);
        }
        if (headers != null && !headers.isEmpty()) {
            buf.putAscii(",\"headers\":{");
            boolean first = true;
            for (Map.Entry<String, String> e : headers.entrySet()) {
                if (!first) {
                    buf.put(',');
                }
                first = false;
                writeJsonString(buf, e.getKey());
                buf.put(':');
                writeJsonString(buf, e.getValue());
            }
            buf.put('}');
        }
        if (appName != null && !appName.isBlank()) {
            buf.putAscii(",\"app\":");
            writeJsonString(buf, appName.trim());
        }
        buf.put('}');
        return buf;
    }

    static ExposedByteArrayOutputStream fillHeaderJson(String appName, String method,
            String path, String query, HeaderSource headers) {
        ExposedByteArrayOutputStream buf = reusableHeaderBuffer();
        // {"v":<WIRE_VERSION>, ...} — WIRE_VERSION is a single decimal digit.
        buf.putAscii("{\"v\":");
        buf.put('0' + WIRE_VERSION);
        buf.putAscii(",\"method\":");
        writeJsonString(buf, method);
        buf.putAscii(",\"path\":");
        writeJsonString(buf, path);
        if (query != null && !query.isEmpty()) {
            buf.putAscii(",\"query\":");
            writeJsonString(buf, query);
        }
        if (headers != null) {
            HeaderJsonSink sink = new HeaderJsonSink(buf);
            headers.writeTo(sink);
            if (sink.started) {
                buf.put('}');
            }
        }
        if (appName != null && !appName.isBlank()) {
            buf.putAscii(",\"app\":");
            writeJsonString(buf, appName.trim());
        }
        buf.put('}');
        return buf;
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
        out.put('"');
    }

    // ── Decode ─────────────────────────────────────────────────────────

    /**
     * Decode a wire-format response.
     *
     * @throws IllegalArgumentException if the wire bytes are malformed
     */
    static DecodedResponse decodeResponse(byte[] wire) {
        if (wire == null || wire.length < 4) {
            throw new IllegalArgumentException(
                    "wire response too short: "
                            + (wire == null ? "null" : wire.length + " bytes"));
        }
        int headerLen = ((wire[0] & 0xFF) << 24) | ((wire[1] & 0xFF) << 16)
                | ((wire[2] & 0xFF) << 8) | (wire[3] & 0xFF);
        if (headerLen < 0 || (long) 4 + headerLen > wire.length) {
            throw new IllegalArgumentException(
                    "wire header_len " + headerLen
                            + " overflows response (" + wire.length + " bytes)");
        }
        // Manual decode via the allocation-lean WireHeaderReader tokenizer
        // (the same parser the DIRECT / streaming header callbacks use)
        // instead of a Jackson JsonParser — drops the per-response parser +
        // IOContext allocation.  Output is shape-identical: status (default
        // 500), headers (String | List<String>), metadata (pre-sized),
        // validation_errors, and unknown fields (incl. "v") skipped.
        WireHeaderReader.Decoded d =
                WireHeaderReader.decode(ByteBuffer.wrap(wire), 4, headerLen);
        ByteBuffer body = ByteBuffer.wrap(wire, 4 + headerLen, wire.length - 4 - headerLen);
        return new DecodedResponse(
                d.status,
                d.headers == null ? Map.of() : d.headers,
                d.metadata,
                body,
                d.validationErrors);
    }
}
