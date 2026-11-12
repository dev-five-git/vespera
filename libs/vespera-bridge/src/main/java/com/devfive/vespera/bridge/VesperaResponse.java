package com.devfive.vespera.bridge;

import java.nio.ByteBuffer;
import java.nio.charset.Charset;
import java.nio.charset.IllegalCharsetNameException;
import java.nio.charset.StandardCharsets;
import java.nio.charset.UnsupportedCharsetException;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import org.jspecify.annotations.NullMarked;
import org.jspecify.annotations.Nullable;

import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;

/**
 * Response of a {@link VesperaClient} request: status, headers, raw body and
 * the hoisted {@code 422} validation errors.
 *
 * <p>The body is exposed as bytes or text only — the bridge never parses
 * domain JSON (Rust owns the business logic). Hand {@link #bodyBytes()} /
 * {@link #bodyString()} to whatever JSON mapper the application already uses.
 *
 * <p>Header values follow the wire's Latin-1 mapping (one {@code char} per
 * header byte, see {@link WireHeaderStringSupport}); ASCII values read as-is.
 * Instances are immutable and thread-safe.
 */
@NullMarked
public final class VesperaResponse {

    private final DecodedResponse decoded;

    VesperaResponse(DecodedResponse decoded) {
        this.decoded = Objects.requireNonNull(decoded, "decoded");
    }

    /** HTTP status code the Rust router answered. */
    public int status() {
        return decoded.status();
    }

    /** Whether {@link #status()} is {@code 2xx}. */
    public boolean isSuccessful() {
        int status = decoded.status();
        return status >= 200 && status < 300;
    }

    /**
     * All response headers, keyed by lower-case name. A value is a
     * {@link String} (single-valued) or a {@code List<String>} (repeated,
     * e.g. {@code set-cookie}). Immutable.
     */
    public Map<String, Object> headers() {
        return decoded.headers();
    }

    /**
     * First value of header {@code name} (case-insensitive), or {@code null}
     * when the response does not carry it.
     */
    public @Nullable String header(String name) {
        Object value = rawHeader(name);
        if (value instanceof List<?> list) {
            return list.isEmpty() ? null : (String) list.get(0);
        }
        return (String) value;
    }

    /** Every value of header {@code name} (case-insensitive); empty when absent. */
    public List<String> headerValues(String name) {
        Object value = rawHeader(name);
        if (value == null) {
            return List.of();
        }
        if (value instanceof List<?> list) {
            @SuppressWarnings("unchecked")
            List<String> values = (List<String>) list;
            return values;
        }
        return List.of((String) value);
    }

    private @Nullable Object rawHeader(String name) {
        Objects.requireNonNull(name, "name");
        Map<String, Object> headers = decoded.headers();
        Object value = headers.get(name);
        return value != null ? value : headers.get(name.toLowerCase(Locale.ROOT));
    }

    /** The {@code content-type} header, or {@code null}. */
    public @Nullable String contentType() {
        return header("content-type");
    }

    /** Read-only, zero-copy view of the body (position 0, limit = length). */
    public ByteBuffer body() {
        return decoded.body();
    }

    /** The body as an owned {@code byte[]} copy. */
    public byte[] bodyBytes() {
        return decoded.bodyBytes();
    }

    /**
     * The body decoded with the {@code charset} parameter of
     * {@code content-type}, falling back to UTF-8 when it is absent or
     * unknown.
     */
    public String bodyString() {
        return bodyString(charsetOf(contentType()));
    }

    /** The body decoded with {@code charset}. */
    public String bodyString(Charset charset) {
        Objects.requireNonNull(charset, "charset");
        ByteBuffer view = decoded.body();
        return charset.decode(view).toString();
    }

    /**
     * Validation failures hoisted from a {@code 422} response
     * ({@code Validated<T>} on the Rust side) — each entry typically carries
     * {@code path} and {@code message}. Empty for any other response.
     */
    public List<Map<String, Object>> validationErrors() {
        List<Map<String, Object>> errors = decoded.validationErrors();
        return errors == null ? List.of() : errors;
    }

    /** Whether the response carries hoisted validation errors. */
    public boolean hasValidationErrors() {
        List<Map<String, Object>> errors = decoded.validationErrors();
        return errors != null && !errors.isEmpty();
    }

    /** Vespera response metadata (e.g. {@code version}); immutable. */
    public Map<String, String> metadata() {
        return decoded.metadata();
    }

    /** Whether the Rust handler panicked (a {@code 500} with {@code panicked=true} metadata). */
    public boolean panicked() {
        return "true".equals(decoded.metadata().get("panicked"));
    }

    /** The underlying low-level {@link DecodedResponse}. */
    public DecodedResponse decoded() {
        return decoded;
    }

    @Override
    public String toString() {
        return "VesperaResponse{status=" + decoded.status()
                + ", headers=" + decoded.headers()
                + ", bodyBytes=" + decoded.body().remaining() + '}';
    }

    /** The {@code charset} parameter of a media type, or UTF-8. */
    static Charset charsetOf(@Nullable String contentType) {
        if (contentType == null) {
            return StandardCharsets.UTF_8;
        }
        for (String param : contentType.split(";")) {
            String p = param.trim();
            if (p.regionMatches(true, 0, "charset=", 0, 8)) {
                String name = p.substring(8).trim();
                if (name.length() >= 2 && name.charAt(0) == '"' && name.charAt(name.length() - 1) == '"') {
                    name = name.substring(1, name.length() - 1);
                }
                try {
                    return Charset.forName(name);
                } catch (IllegalCharsetNameException | UnsupportedCharsetException e) {
                    return StandardCharsets.UTF_8;
                }
            }
        }
        return StandardCharsets.UTF_8;
    }
}
