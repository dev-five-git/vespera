package com.devfive.vespera.bridge;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.CharBuffer;
import java.nio.charset.Charset;
import java.nio.charset.CharsetEncoder;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;
import org.jspecify.annotations.NullMarked;
import org.jspecify.annotations.Nullable;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;

/**
 * Request builder returned by {@link VesperaClient}'s {@code get}/{@code post}/...
 * methods; finish it with {@link #send()} or {@link #sendAsync()}.
 *
 * <p>Header names are lower-cased; {@link #header} replaces a value,
 * {@link #addHeader} joins repeated values with {@code ", "}
 * ({@code "; "} for {@code cookie}) — the wire carries one value per name,
 * like the proxy. Query parameters are percent-encoded (UTF-8, RFC 3986;
 * a space becomes {@code %20}).
 *
 * <p>Header values use the wire's Latin-1 mapping, like a servlet container:
 * a value whose characters are all {@code <= U+00FF} travels as one byte per
 * character, so {@code "café"} reaches Rust as the single byte
 * {@code 0xE9} (not UTF-8; {@code HeaderValue::to_str()} rejects it), while a
 * value with any wider character is sent as UTF-8. Percent- or RFC 8187-encode
 * non-ASCII text for a portable result.
 *
 * <p>The body is raw bytes: {@link #json(String)} sends the string as-is
 * with {@code content-type: application/json} — the bridge never serialises
 * or parses domain JSON. A builder is not thread-safe; a buffered request
 * may be sent repeatedly, an {@link InputStream} body only once.
 */
@NullMarked
public final class VesperaRequest {

    private final VesperaClient.Transport transport;
    private final String method;
    private final String path;
    private final StringBuilder query = new StringBuilder();
    private final Map<String, String> headers = new LinkedHashMap<>();
    private String appName;
    private byte[] body = VesperaWireCodec.EMPTY_BODY;
    private InputStream bodyStream;
    private boolean streamConsumed;
    private String remoteAddr;
    private String scheme;
    private VesperaPrincipal principal;

    VesperaRequest(VesperaClient.Transport transport, @Nullable String appName, String method, String path) {
        this.transport = transport;
        this.appName = appName;
        this.method = requireToken(Objects.requireNonNull(method, "method"), "method");
        Objects.requireNonNull(path, "path");
        if (path.isEmpty() || path.charAt(0) != '/') {
            throw new IllegalArgumentException("path must start with '/': " + path);
        }
        if (path.indexOf('#') >= 0) {
            throw new IllegalArgumentException("path must not contain a fragment ('#'): " + path);
        }
        int q = path.indexOf('?');
        if (q >= 0) {
            this.path = path.substring(0, q);
            query.append(path, q + 1, path.length());
        } else {
            this.path = path;
        }
    }

    // ── Target ───────────────────────────────────────────────────────

    /**
     * Route this request to the named app instead of the client's
     * ({@code null} / blank / {@code "_default"} = the default app).
     */
    public VesperaRequest app(@Nullable String appName) {
        this.appName = VesperaClient.clientAppName(appName);
        return this;
    }

    /**
     * Append the query parameter {@code name=value}, both percent-encoded.
     * A {@code null} value appends the bare {@code name}; other values use
     * {@link String#valueOf(Object)}. Repeat the call for repeated keys.
     */
    public VesperaRequest query(String name, @Nullable Object value) {
        Objects.requireNonNull(name, "name");
        appendQuerySeparator();
        query.append(VesperaClient.percentEncode(name));
        if (value != null) {
            query.append('=').append(VesperaClient.percentEncode(String.valueOf(value)));
        }
        return this;
    }

    /** Append every entry of {@code params} as {@link #query(String, Object)}. */
    public VesperaRequest query(Map<String, ?> params) {
        Objects.requireNonNull(params, "params").forEach(this::query);
        return this;
    }

    /**
     * Append an already-encoded query fragment verbatim (e.g. {@code "a=1&b=2"}).
     *
     * @throws IllegalArgumentException when it contains {@code '#'} (send it as {@code %23})
     */
    public VesperaRequest rawQuery(String encodedQuery) {
        Objects.requireNonNull(encodedQuery, "encodedQuery");
        if (encodedQuery.indexOf('#') >= 0) {
            // The URI parser would cut the query at '#' (a fragment) and drop the rest.
            throw new IllegalArgumentException("query must not contain a fragment ('#'): " + encodedQuery);
        }
        if (!encodedQuery.isEmpty()) {
            appendQuerySeparator();
            query.append(encodedQuery);
        }
        return this;
    }

    private void appendQuerySeparator() {
        if (query.length() > 0) {
            query.append('&');
        }
    }

    // ── Headers ──────────────────────────────────────────────────────

    /**
     * Set header {@code name} (case-insensitive) to {@code value}, replacing any
     * previous value. Non-ASCII values follow the Latin-1 mapping (class docs).
     */
    public VesperaRequest header(String name, String value) {
        headers.put(headerName(name), checkHeaderValue(value));
        return this;
    }

    /**
     * Add a value to header {@code name}, joining with {@code ", "} ({@code "; "}
     * for cookie). Non-ASCII values follow the Latin-1 mapping (class docs).
     */
    public VesperaRequest addHeader(String name, String value) {
        String key = headerName(name);
        String v = checkHeaderValue(value);
        headers.merge(key, v, (a, b) -> a + (key.equals("cookie") ? "; " : ", ") + b);
        return this;
    }

    /** {@link #header(String, String)} for every entry. */
    public VesperaRequest headers(Map<String, String> headers) {
        Objects.requireNonNull(headers, "headers").forEach(this::header);
        return this;
    }

    /** Remove header {@code name}. */
    public VesperaRequest removeHeader(String name) {
        headers.remove(headerName(name));
        return this;
    }

    /**
     * Client socket address forwarded as the {@code remote_addr} wire field
     * ({@code ConnectInfo<SocketAddr>} in Rust): {@code ip:port}, IPv6 as
     * {@code [ip]:port}. Unparseable values are ignored by Rust.
     */
    public VesperaRequest remoteAddr(@Nullable String remoteAddr) {
        this.remoteAddr = remoteAddr;
        return this;
    }

    /** {@code http} / {@code https}, forwarded as the {@code scheme} wire field. */
    public VesperaRequest scheme(@Nullable String scheme) {
        this.scheme = scheme;
        return this;
    }

    /**
     * Trusted principal forwarded as the {@code principal} wire field (Rust
     * extracts {@code vespera::inprocess::Principal}); {@code null} = anonymous.
     */
    public VesperaRequest principal(@Nullable VesperaPrincipal principal) {
        this.principal = principal;
        return this;
    }

    // ── Body ─────────────────────────────────────────────────────────

    /** Raw JSON text (sent as UTF-8, not parsed) with {@code content-type: application/json}. */
    public VesperaRequest json(String json) {
        return body(VesperaClient.utf8(Objects.requireNonNull(json, "json")),
                "application/json");
    }

    /** Raw JSON bytes (not parsed) with {@code content-type: application/json}. */
    public VesperaRequest json(byte[] json) {
        return body(json, "application/json");
    }

    /** UTF-8 text with {@code content-type: text/plain; charset=utf-8}. */
    public VesperaRequest text(String text) {
        return body(text, "text/plain; charset=utf-8");
    }

    /**
     * {@code application/x-www-form-urlencoded} body from {@code fields}
     * (percent-encoded UTF-8; a {@code null} value sends the bare name).
     */
    public VesperaRequest form(Map<String, ?> fields) {
        StringBuilder sb = new StringBuilder();
        Objects.requireNonNull(fields, "fields").forEach((name, value) -> {
            if (sb.length() > 0) {
                sb.append('&');
            }
            sb.append(VesperaClient.percentEncode(Objects.requireNonNull(name, "field name")));
            if (value != null) {
                sb.append('=').append(VesperaClient.percentEncode(String.valueOf(value)));
            }
        });
        return body(sb.toString().getBytes(StandardCharsets.US_ASCII),
                "application/x-www-form-urlencoded");
    }

    /**
     * Text body encoded with the {@code charset} parameter of
     * {@code contentType} (UTF-8 when absent). For a charset other than UTF-8
     * the encoding is strict: a character the charset cannot represent is
     * never replaced by a substitute byte such as {@code '?'}; the call fails
     * instead, so the server never receives data the caller did not write
     * (UTF-8 keeps the client's U+FFFD rule for unpaired surrogates).
     *
     * @param contentType sent as {@code content-type}; {@code null} leaves the header alone
     * @throws IllegalArgumentException when the {@code charset} parameter names a charset this JVM
     *     does not know or cannot encode (the bytes would not match the declared label), or when
     *     the text has a character the charset cannot encode (the message names it and its index)
     */
    public VesperaRequest body(String text, @Nullable String contentType) {
        Objects.requireNonNull(text, "text");
        String declared = VesperaResponse.charsetParameter(contentType);
        java.nio.charset.Charset charset = StandardCharsets.UTF_8;
        if (declared != null) {
            try {
                charset = java.nio.charset.Charset.forName(declared);
            } catch (java.nio.charset.IllegalCharsetNameException
                    | java.nio.charset.UnsupportedCharsetException unknown) {
                throw new IllegalArgumentException("the charset '" + declared + "' of content type '"
                        + contentType + "' is not known to this JVM", unknown);
            }
        }
        if (!charset.canEncode()) {
            throw new IllegalArgumentException("the charset " + charset.name() + " of content type '"
                    + contentType + "' can only decode; send the body as bytes or use another charset");
        }
        // UTF-8 keeps the client's documented rule (an unpaired surrogate becomes
        // U+FFFD, see VesperaClient#utf8); every other charset fails on what it cannot encode.
        byte[] bytes = charset.equals(StandardCharsets.UTF_8) ? VesperaClient.utf8(text) : encodeStrict(charset, text);
        return body(bytes, contentType);
    }

    /**
     * Encodes {@code text} with a non-UTF-8 {@code charset}, reporting (not
     * substituting) the first character it cannot represent: {@link String#getBytes}
     * would write a {@code '?'} byte for it.
     */
    private static byte[] encodeStrict(Charset charset, String text) {
        CharsetEncoder encoder = charset.newEncoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        CharBuffer in = CharBuffer.wrap(text);
        ByteBuffer out = ByteBuffer.allocate((int) Math.ceil(text.length() * (double) encoder.maxBytesPerChar()));
        if (encoder.encode(in, out, true).isError()) {
            throw new IllegalArgumentException(String.format(
                    "the text has U+%04X at index %d, which the charset %s cannot encode",
                    text.codePointAt(in.position()), in.position(), charset.name()));
        }
        encoder.flush(out);
        out.flip();
        byte[] bytes = new byte[out.remaining()];
        out.get(bytes);
        return bytes;
    }

    /**
     * Raw body bytes (used as-is, not copied).
     *
     * @param contentType sent as {@code content-type}; {@code null} leaves the header alone
     */
    public VesperaRequest body(byte[] bytes, @Nullable String contentType) {
        Objects.requireNonNull(bytes, "bytes");
        // The header first: a rejected content type (control characters) throws
        // before the body changes, so a caller that catches it keeps its old body.
        if (contentType != null) {
            header("content-type", contentType);
        }
        this.body = bytes;
        this.bodyStream = null;
        return this;
    }

    /**
     * Streamed request body: pulled chunk-by-chunk by Rust through the
     * bidirectional streaming path (no full copy in memory; capped by
     * {@code vespera.limits.maxStreamingRequestBytes}). The response is
     * still collected in memory. The native side closes the stream once the
     * dispatch is done with it (after the response drained, when the handler
     * read from it); a read still blocked in it then fails with that close,
     * which does not fail the send. Callers should not rely on the stream
     * staying open, and it can be sent once; {@link #sendAsync()} needs an
     * {@link Executor}
     * ({@link #sendAsync(Executor)}) because the call blocks.
     *
     * @param contentType sent as {@code content-type}; {@code null} leaves the header alone
     */
    public VesperaRequest body(InputStream stream, @Nullable String contentType) {
        Objects.requireNonNull(stream, "stream");
        // Header first, as in body(byte[], ...): a rejected content type leaves
        // the previous body in place.
        if (contentType != null) {
            header("content-type", contentType);
        }
        this.bodyStream = stream;
        this.streamConsumed = false;
        this.body = VesperaWireCodec.EMPTY_BODY;
        return this;
    }

    // ── Send ─────────────────────────────────────────────────────────

    /**
     * Dispatch synchronously (SYNC; parks instead of pinning on a virtual
     * thread) and return the response — any HTTP status included.
     *
     * @throws VesperaClientException on a transport failure (library not
     *         initialised, malformed wire response, streamed body failure)
     * @throws IllegalArgumentException when the encoded request cannot be
     *         buffered: a wire past 2 GiB, or a header block past 64 MiB (the
     *         cases {@link #sendAsync()} completes its future with)
     */
    public VesperaResponse send() {
        if (bodyStream != null) {
            return sendStreaming();
        }
        byte[] wire = encode(body);
        byte[] response;
        try {
            response = transport.dispatch(wire);
        } catch (UnsatisfiedLinkError e) {
            throw nativeMissing(e);
        }
        return decode(response);
    }

    /**
     * Dispatch asynchronously (ASYNC, {@link VesperaBridge#dispatchAsync}).
     * The future completes on a Rust Tokio worker thread — attach blocking
     * or heavy continuations with the {@code *Async} variants, and never call
     * a blocking vespera dispatch ({@link #send()}) inline from one; or use
     * {@link #sendAsync(Executor)}. Transport failures complete the future
     * exceptionally with {@link VesperaClientException}. A request that cannot
     * be encoded (for example a header block past the 64 MiB ceiling) completes
     * it the same way, with an {@link IllegalArgumentException}. Cancelling the
     * returned future (or completing it exceptionally, e.g. via
     * {@code orTimeout}) cancels the native dispatch: the Rust handler is
     * dropped and its in-flight slot released.
     *
     * @throws IllegalStateException for an {@link InputStream} body (use
     *         {@link #sendAsync(Executor)})
     */
    public CompletableFuture<VesperaResponse> sendAsync() {
        if (bodyStream != null) {
            throw new IllegalStateException(
                    "a streamed (InputStream) body blocks while it is read; use sendAsync(Executor)");
        }
        CompletableFuture<byte[]> raw = dispatchAsync();
        return propagateCancellation(raw, raw.thenApply(VesperaRequest::decode));
    }

    /**
     * {@link #sendAsync()} with the response decoded — and dependent stages
     * run — on {@code executor} instead of the Tokio worker. An
     * {@link InputStream} body runs the blocking {@link #send()} on it.
     */
    public CompletableFuture<VesperaResponse> sendAsync(Executor executor) {
        Objects.requireNonNull(executor, "executor");
        if (bodyStream != null) {
            return CompletableFuture.supplyAsync(this::send, executor);
        }
        CompletableFuture<byte[]> raw = dispatchAsync();
        return propagateCancellation(raw, raw.thenApplyAsync(VesperaRequest::decode, executor));
    }

    /**
     * A dependent stage does not cancel its source: once {@code decoded} (the
     * caller's future) completes exceptionally, cancel {@code raw} as well so
     * the native dispatch behind it is aborted. A no-op when {@code raw} is
     * already done (the response arrived, or the failure came from it).
     */
    static <T> CompletableFuture<T> propagateCancellation(
            CompletableFuture<byte[]> raw, CompletableFuture<T> decoded) {
        decoded.whenComplete((ignored, failure) -> {
            if (failure != null) {
                raw.cancel(false);
            }
        });
        return decoded;
    }

    private CompletableFuture<byte[]> dispatchAsync() {
        byte[] wire;
        try {
            wire = encode(body);
        } catch (IllegalArgumentException e) {
            return CompletableFuture.failedFuture(e);
        }
        try {
            return transport.dispatchAsync(wire);
        } catch (VesperaClientException e) {
            return CompletableFuture.failedFuture(e);
        } catch (UnsatisfiedLinkError e) {
            return CompletableFuture.failedFuture(nativeMissing(e));
        }
    }

    private VesperaResponse sendStreaming() {
        if (streamConsumed) {
            throw new IllegalStateException("the InputStream body was already sent");
        }
        // Encode before marking the stream consumed: a failed encode never
        // touched the body, so the caller may fix the request and send it again.
        byte[] header = encode(VesperaWireCodec.EMPTY_BODY);
        streamConsumed = true;
        ExposedOutput out = new ExposedOutput();
        ReadTracking tracked = new ReadTracking(bodyStream);
        byte[] responseHeader;
        try {
            responseHeader = transport.dispatchStreaming(header, tracked, out);
        } catch (IOException e) {
            // Same rule as the unchecked branches: a dispatch that failed
            // before the request body was read leaves it sendable again (unless
            // the native side closed it, see releaseUnreadStream).
            releaseUnreadStream(tracked);
            VesperaClientException failure = new VesperaClientException(
                    "streamed response failed "
                            + (out.size() > 0 ? "after body bytes were written"
                                    : "before any body byte was written"),
                    e);
            if (tracked.readFailure != null && tracked.readFailure != e) {
                failure.addSuppressed(tracked.readFailure);
            }
            throw failure;
        } catch (UnsatisfiedLinkError e) {
            releaseUnreadStream(tracked);
            throw nativeMissing(e);
        } catch (RuntimeException e) {
            // e.g. "native library is not initialized": thrown before the
            // body was touched, so the same request may be sent again.
            releaseUnreadStream(tracked);
            throw e;
        }
        IOException readFailure = tracked.readFailure;
        if (readFailure != null) {
            // The native side turned it into a request-body error and the
            // handler answered something (400 / 500 / even 200): the response
            // does not describe the request the caller meant to send.
            throw new VesperaClientException("reading the request body InputStream failed", readFailure);
        }
        VesperaResponse head = decode(responseHeader);
        if (out.size() == 0) {
            // Nothing streamed: either an empty body or an error wire that
            // carries its own (small) body inline.
            return head;
        }
        DecodedResponse d = head.decoded();
        return new VesperaResponse(new DecodedResponse(
                d.status(), d.headers(), d.metadata(), out.view(), d.validationErrors()));
    }

    /** Encode this request (with {@code payload} as body) via the shared wire codec. */
    byte[] encode(byte[] payload) {
        Map<String, String> snapshot = headers;
        String addr = remoteAddr;
        String sch = scheme;
        VesperaPrincipal who = principal;
        VesperaBridge.HeaderSource source = new VesperaBridge.HeaderSource() {
            @Override
            public void writeTo(VesperaBridge.HeaderSink sink) {
                snapshot.forEach(sink::put);
            }

            @Override
            public String remoteAddr() {
                return addr;
            }

            @Override
            public String scheme() {
                return sch;
            }

            @Override
            public VesperaPrincipal principal() {
                return who;
            }
        };
        return VesperaWireCodec.encodeRequest(
                appName, method, path, query.length() == 0 ? null : query.toString(), source, payload);
    }

    static VesperaResponse decode(byte[] wire) {
        if (wire == null) {
            throw new VesperaClientException("native dispatch returned no wire response");
        }
        try {
            return new VesperaResponse(VesperaWireCodec.decodeResponse(wire));
        } catch (RuntimeException e) {
            throw new VesperaClientException("malformed wire response (" + wire.length + " bytes)", e);
        }
    }

    /**
     * A failed send that never read the body leaves the stream sendable again.
     * A stream the native side closed during the dispatch is NOT re-armed,
     * even one nothing read: {@link ReadTracking#close()} counts as a use. The
     * dispatch took the stream and closed it when it finished, so a resend
     * would read a stream the native side has already released.
     */
    private void releaseUnreadStream(ReadTracking tracked) {
        if (!tracked.touched) {
            streamConsumed = false;
        }
    }

    /**
     * Remembers whether the transport touched the request body stream at all,
     * and the first {@link IOException} reading it threw: the native side
     * swallows that exception (it only ends the request body with an error),
     * so {@link #sendStreaming()} rethrows it after the dispatch.
     */
    static final class ReadTracking extends java.io.FilterInputStream {
        /** Set by any read, skip or close (a close by the native side is a use too). */
        volatile boolean touched;
        volatile IOException readFailure;
        /**
         * Set by {@link #close()} BEFORE the delegate closes. The native side
         * closes the source once the response drained (the handler will not
         * read more); a read still parked on another thread then fails
         * because of that close (e.g. {@code PipedInputStream}: "Pipe
         * closed"), which is not a request-body failure.
         */
        volatile boolean closed;

        ReadTracking(InputStream in) {
            super(in);
        }

        @Override
        public int read() throws IOException {
            touched = true;
            try {
                return super.read();
            } catch (IOException e) {
                recordFailure(e);
                throw e;
            }
        }

        @Override
        public int read(byte[] b, int off, int len) throws IOException {
            touched = true;
            try {
                return in.read(b, off, len);
            } catch (IOException e) {
                recordFailure(e);
                throw e;
            }
        }

        private void recordFailure(IOException e) {
            if (readFailure == null && !closed) {
                readFailure = e;
            }
        }

        @Override
        public long skip(long n) throws IOException {
            touched = true;
            return super.skip(n);
        }

        @Override
        public void close() throws IOException {
            touched = true;
            closed = true;
            super.close();
        }
    }

    private static VesperaClientException nativeMissing(UnsatisfiedLinkError e) {
        return new VesperaClientException(
                "vespera native dispatch symbol is unavailable (library not loaded or "
                        + "from another release): " + e.getMessage(), e);
    }

    // ── Validation ───────────────────────────────────────────────────

    private static String headerName(String name) {
        return requireToken(Objects.requireNonNull(name, "header name"), "header name")
                .toLowerCase(Locale.ROOT);
    }

    private static String checkHeaderValue(String value) {
        Objects.requireNonNull(value, "header value");
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            // RFC 9110 field-value: no control characters except HTAB
            // (CR / LF / NUL split or truncate the header; the others are
            // rejected by servers and proxies alike).
            if ((c < 0x20 && c != '\t') || c == 0x7F) {
                throw new IllegalArgumentException(
                        "header value must not contain control characters (except HTAB): 0x"
                                + Integer.toHexString(c));
            }
        }
        return value;
    }

    /** RFC 9110 token ({@code tchar}+), used for method and header names. */
    private static String requireToken(String s, String what) {
        if (s.isEmpty()) {
            throw new IllegalArgumentException(what + " must not be empty");
        }
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            boolean tchar = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')
                    || "!#$%&'*+-.^_`|~".indexOf(c) >= 0;
            if (!tchar) {
                throw new IllegalArgumentException(what + " is not a valid HTTP token: " + s);
            }
        }
        return s;
    }

    // ── Accessors (diagnostics / tests) ──────────────────────────────

    /** HTTP method. */
    public String method() {
        return method;
    }

    /** Path without the query. */
    public String path() {
        return path;
    }

    /** Encoded query string ({@code ""} when none). */
    public String queryString() {
        return query.toString();
    }

    /** Target app name, or {@code null} for the default app. */
    public @Nullable String appName() {
        return appName;
    }

    /** Current header value for {@code name}, or {@code null}. */
    public @Nullable String headerValue(String name) {
        return headers.get(Objects.requireNonNull(name, "name").toLowerCase(Locale.ROOT));
    }

    /** A {@link ByteArrayOutputStream} whose bytes become the body view without a copy. */
    private static final class ExposedOutput extends ByteArrayOutputStream {
        ExposedOutput() {
            super(8192);
        }

        ByteBuffer view() {
            return ByteBuffer.wrap(buf, 0, count).slice().asReadOnlyBuffer();
        }
    }
}
