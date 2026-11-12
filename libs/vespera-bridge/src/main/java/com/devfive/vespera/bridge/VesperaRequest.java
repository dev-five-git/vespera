package com.devfive.vespera.bridge;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;

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
 * <p>The body is raw bytes: {@link #json(String)} sends the string as-is
 * with {@code content-type: application/json} — the bridge never serialises
 * or parses domain JSON. A builder is not thread-safe; a buffered request
 * may be sent repeatedly, an {@link InputStream} body only once.
 */
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

    VesperaRequest(VesperaClient.Transport transport, String appName, String method, String path) {
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
     * ({@code null} / blank = the default app).
     */
    public VesperaRequest app(String appName) {
        this.appName = VesperaWireCodec.normalizedAppName(appName);
        return this;
    }

    /**
     * Append the query parameter {@code name=value}, both percent-encoded.
     * A {@code null} value appends the bare {@code name}; other values use
     * {@link String#valueOf(Object)}. Repeat the call for repeated keys.
     */
    public VesperaRequest query(String name, Object value) {
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

    /** Append an already-encoded query fragment verbatim (e.g. {@code "a=1&b=2"}). */
    public VesperaRequest rawQuery(String encodedQuery) {
        Objects.requireNonNull(encodedQuery, "encodedQuery");
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

    /** Set header {@code name} (case-insensitive) to {@code value}, replacing any previous value. */
    public VesperaRequest header(String name, String value) {
        headers.put(headerName(name), checkHeaderValue(value));
        return this;
    }

    /** Add a value to header {@code name}, joining with {@code ", "} ({@code "; "} for cookie). */
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
    public VesperaRequest remoteAddr(String remoteAddr) {
        this.remoteAddr = remoteAddr;
        return this;
    }

    /** {@code http} / {@code https}, forwarded as the {@code scheme} wire field. */
    public VesperaRequest scheme(String scheme) {
        this.scheme = scheme;
        return this;
    }

    /**
     * Trusted principal forwarded as the {@code principal} wire field (Rust
     * extracts {@code vespera::inprocess::Principal}); {@code null} = anonymous.
     */
    public VesperaRequest principal(VesperaPrincipal principal) {
        this.principal = principal;
        return this;
    }

    // ── Body ─────────────────────────────────────────────────────────

    /** Raw JSON text (sent as UTF-8, not parsed) with {@code content-type: application/json}. */
    public VesperaRequest json(String json) {
        return body(Objects.requireNonNull(json, "json").getBytes(StandardCharsets.UTF_8),
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
     * {@code contentType} (UTF-8 when absent).
     *
     * @param contentType sent as {@code content-type}; {@code null} leaves the header alone
     */
    public VesperaRequest body(String text, String contentType) {
        Objects.requireNonNull(text, "text");
        return body(text.getBytes(VesperaResponse.charsetOf(contentType)), contentType);
    }

    /**
     * Raw body bytes (used as-is, not copied).
     *
     * @param contentType sent as {@code content-type}; {@code null} leaves the header alone
     */
    public VesperaRequest body(byte[] bytes, String contentType) {
        this.body = Objects.requireNonNull(bytes, "bytes");
        this.bodyStream = null;
        if (contentType != null) {
            header("content-type", contentType);
        }
        return this;
    }

    /**
     * Streamed request body: pulled chunk-by-chunk by Rust through the
     * bidirectional streaming path (no full copy in memory; capped by
     * {@code vespera.limits.maxStreamingRequestBytes}). The response is
     * still collected in memory. The stream is read but not closed, and can
     * be sent once; {@link #sendAsync()} needs an {@link Executor}
     * ({@link #sendAsync(Executor)}) because the call blocks.
     *
     * @param contentType sent as {@code content-type}; {@code null} leaves the header alone
     */
    public VesperaRequest body(InputStream stream, String contentType) {
        this.bodyStream = Objects.requireNonNull(stream, "stream");
        this.streamConsumed = false;
        this.body = VesperaWireCodec.EMPTY_BODY;
        if (contentType != null) {
            header("content-type", contentType);
        }
        return this;
    }

    // ── Send ─────────────────────────────────────────────────────────

    /**
     * Dispatch synchronously (SYNC; parks instead of pinning on a virtual
     * thread) and return the response — any HTTP status included.
     *
     * @throws VesperaClientException on a transport failure (library not
     *         initialised, malformed wire response, streamed body failure)
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
     * exceptionally with {@link VesperaClientException}.
     *
     * @throws IllegalStateException for an {@link InputStream} body (use
     *         {@link #sendAsync(Executor)})
     */
    public CompletableFuture<VesperaResponse> sendAsync() {
        if (bodyStream != null) {
            throw new IllegalStateException(
                    "a streamed (InputStream) body blocks while it is read; use sendAsync(Executor)");
        }
        return dispatchAsync().thenApply(VesperaRequest::decode);
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
        return dispatchAsync().thenApplyAsync(VesperaRequest::decode, executor);
    }

    private CompletableFuture<byte[]> dispatchAsync() {
        byte[] wire = encode(body);
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
        streamConsumed = true;
        byte[] header = encode(VesperaWireCodec.EMPTY_BODY);
        ExposedOutput out = new ExposedOutput();
        byte[] responseHeader;
        try {
            responseHeader = transport.dispatchStreaming(header, bodyStream, out);
        } catch (IOException e) {
            throw new VesperaClientException(
                    "streamed response failed after body bytes were written", e);
        } catch (UnsatisfiedLinkError e) {
            throw nativeMissing(e);
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
            if (c == '\r' || c == '\n' || c == '\0') {
                throw new IllegalArgumentException("header value must not contain CR, LF or NUL");
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
    public String appName() {
        return appName;
    }

    /** Current header value for {@code name}, or {@code null}. */
    public String headerValue(String name) {
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
