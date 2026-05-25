package com.devfive.vespera.bridge;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Objects;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.function.Consumer;

/**
 * JNI bridge to any Rust cdylib built with vespera's JNI feature.
 *
 * <p><strong>Wire format</strong> — both request and response use the
 * same layout:
 * <pre>
 *   bytes 0..4    : u32 BE = header_json byte length N
 *   bytes 4..4+N  : UTF-8 JSON header
 *                     (request)  { "v":1, "method", "path",
 *                                  "query"?, "headers"? }
 *                     (response) { "v":1, "status", "headers",
 *                                  "metadata" }
 *   bytes 4+N..   : raw body bytes (UTF-8 text or binary —
 *                   no encoding applied)
 * </pre>
 *
 * <p><strong>Usage</strong> — single line in your Spring Boot app:
 * <pre>{@code
 * VesperaBridge.init("rust_jni_demo");
 * }</pre>
 *
 * <p>The proxy controller ({@link VesperaProxyController}) is
 * auto-configured by Spring's component scan when this JAR is on the
 * classpath.
 */
public class VesperaBridge {

    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static final int WIRE_VERSION = 1;
    private static volatile boolean loaded = false;

    /**
     * Decoded wire-format response.
     *
     * @param status            HTTP status code from the upstream router
     * @param headers           response headers; each value is either a
     *                          {@link String} (single-valued) or a
     *                          {@link List List&lt;String&gt;}
     *                          (multi-valued, e.g. {@code set-cookie})
     * @param metadata          vespera metadata (e.g. {@code version})
     * @param body              raw response body bytes
     * @param validationErrors  Vespera-validation failures hoisted from
     *                          a {@code 422} JSON body so callers can
     *                          read them without a second JSON parse.
     *                          {@code null} when the response is not a
     *                          422 or doesn't carry the {@code
     *                          validation_errors} wire header field.
     *                          Each entry typically has {@code path},
     *                          {@code code}, and {@code message} keys.
     */
    public record DecodedResponse(
            int status,
            Map<String, Object> headers,
            Map<String, String> metadata,
            byte[] body,
            List<Map<String, Object>> validationErrors) {}

    /**
     * Initialize the Rust engine.  Tries bundled (JAR-embedded) first,
     * falls back to {@code java.library.path}.
     *
     * @param libraryName Cargo crate name (e.g. {@code "rust_jni_demo"})
     */
    public static synchronized void init(String libraryName) {
        if (loaded) return;
        try {
            loadBundled(libraryName);
        } catch (UnsatisfiedLinkError e) {
            System.loadLibrary(libraryName);
        }
        loaded = true;
    }

    /**
     * Dispatch a wire-format HTTP-like request through the Rust axum
     * router (<strong>synchronous</strong> — blocks the calling
     * thread).  See {@link VesperaBridge class-level docs} for the
     * wire layout.
     *
     * @param wireRequest length-prefixed binary wire request
     * @return length-prefixed binary wire response
     */
    public static native byte[] dispatchBytes(byte[] wireRequest);

    /**
     * Asynchronous variant of {@link #dispatchBytes(byte[])}.  Returns
     * immediately after spawning the dispatch on Rust's Tokio runtime;
     * the supplied {@link CompletableFuture} is completed with the
     * wire-format response bytes from a runtime worker thread.
     *
     * <p>Contract (always-complete): the future is always completed
     * with a valid wire response.  Panics in the Rust handler are
     * converted to a `500` wire response; JNI conversion failures to
     * a `400` wire response.  The future is never left dangling.
     *
     * <p>Cancellation is not propagated to the Rust task in this
     * release: {@code future.cancel(true)} will mark the future as
     * cancelled on the Java side, but the in-flight Rust dispatch
     * continues to completion (and its result is discarded).
     *
     * @param future        the future to complete with the wire response
     * @param wireRequest   length-prefixed binary wire request
     */
    public static native void dispatchAsync(
            CompletableFuture<byte[]> future, byte[] wireRequest);

    /**
     * Convenience wrapper around {@link #dispatchAsync} that allocates
     * the {@link CompletableFuture} and returns it.
     *
     * @param wireRequest length-prefixed binary wire request
     * @return future that resolves to the wire-format response bytes
     */
    public static CompletableFuture<byte[]> dispatch(byte[] wireRequest) {
        CompletableFuture<byte[]> future = new CompletableFuture<>();
        dispatchAsync(future, wireRequest);
        return future;
    }

    /**
     * <strong>Streaming</strong> binary wire-format JNI dispatch.  The
     * dispatch runs synchronously on the calling thread (like
     * {@link #dispatchBytes(byte[])}) but emits the response body
     * <strong>chunk-by-chunk</strong> to {@code outputStream.write(byte[])}
     * — neither the Rust side nor the JVM ever holds the full body in
     * memory at once.
     *
     * <p>Returns the wire-format <strong>header bytes only</strong>
     * (length-prefixed JSON: status, headers, metadata).  The body
     * arrived via {@code outputStream} while the dispatch was in
     * flight.
     *
     * <p>Failure modes (malformed wire, panic in Rust, no app
     * registered) return a regular {@code error_wire(...)} response
     * (header + small plain-text body) and the {@code outputStream}
     * is <strong>not</strong> written to.  Callers can detect a
     * streaming error by checking whether the returned bytes carry a
     * non-empty body via {@link #decodeResponse(byte[])}.
     *
     * @param wireRequest  length-prefixed binary wire request
     * @param outputStream sink for response body chunks
     * @return wire-format header bytes (body lives on the OutputStream)
     */
    public static native byte[] dispatchStreaming(byte[] wireRequest, OutputStream outputStream);

    /**
     * <strong>Bidirectional streaming</strong> binary wire-format JNI
     * dispatch — both request body (from {@code inputStream}) and
     * response body (to {@code outputStream}) are processed
     * chunk-by-chunk.  Neither side materialises the full body in
     * memory, so a 1 GiB upload paired with a 1 GiB download runs in
     * roughly {@code O(chunk_size)} RAM.
     *
     * <p>Wire envelope contract:
     * <ul>
     *   <li>{@code wireRequestHeader} is a wire-format request
     *       <strong>without a body</strong> — just the 4-byte length
     *       prefix + JSON header (method, path, query, headers).
     *       Use {@link #encodeRequest(String, String, String, java.util.Map, byte[])}
     *       with an empty {@code body} array.</li>
     *   <li>The request body bytes flow through {@code inputStream}
     *       — Rust calls {@code inputStream.read(byte[])} repeatedly
     *       (16 KiB at a time) until EOF.</li>
     *   <li>The response body bytes flow through {@code outputStream}
     *       — Rust calls {@code outputStream.write(byte[])} for each
     *       axum body frame.</li>
     * </ul>
     *
     * <p>Returns the wire-format <strong>header bytes only</strong>
     * (status, headers, metadata).  Decode with
     * {@link #decodeResponse(byte[])} to read the status and headers
     * — the body has already been written to {@code outputStream}.
     *
     * <p>Failure modes (malformed wire, panic in Rust, no app
     * registered) return a regular {@code error_wire(...)} response
     * (header + small plain-text body) and <strong>neither</strong>
     * stream is touched.
     *
     * @param wireRequestHeader length-prefixed binary wire request
     *                          header (no body)
     * @param inputStream       source for request body chunks
     * @param outputStream      sink for response body chunks
     * @return wire-format header bytes (body lives on the
     *         {@code outputStream})
     */
    public static native byte[] dispatchFullStreaming(
            byte[] wireRequestHeader,
            InputStream inputStream,
            OutputStream outputStream);

    /**
     * Convenience encoder for the bidirectional streaming variant —
     * produces a wire-format header with an empty body, suitable for
     * passing to {@link #dispatchFullStreaming(byte[], InputStream, OutputStream)}.
     *
     * @param method  HTTP method (uppercase)
     * @param path    URL path
     * @param query   raw query string (may be {@code null})
     * @param headers request headers
     * @return wire bytes with the JSON header and no body
     */
    public static byte[] encodeRequestHeader(
            String method,
            String path,
            String query,
            java.util.Map<String, String> headers) {
        return encodeRequestHeader(null, method, path, query, headers);
    }

    /**
     * Same as {@link #encodeRequestHeader(String, String, String, java.util.Map)}
     * but with an explicit app name for multi-app routing.  See
     * {@link #encodeRequest(String, String, String, String, java.util.Map, byte[])}
     * for app name semantics.
     */
    public static byte[] encodeRequestHeader(
            String appName,
            String method,
            String path,
            String query,
            java.util.Map<String, String> headers) {
        return encodeRequest(
                appName,
                Objects.requireNonNull(method, "method"),
                Objects.requireNonNull(path, "path"),
                query,
                headers != null ? headers : java.util.Map.of(),
                new byte[0]);
    }

    /**
     * Variant of {@link #dispatchStreaming(byte[], OutputStream)} that
     * emits the wire-format response header via {@code headerConsumer}
     * <strong>before</strong> the first body byte reaches
     * {@code outputStream}.
     *
     * <p>This is the variant Spring {@link jakarta.servlet.http.HttpServletResponse}
     * controllers want: the header callback fires while the response
     * is still uncommitted, so the controller can call
     * {@code resp.setStatus(...)} / {@code resp.setHeader(...)} from
     * inside {@code headerConsumer.accept(byte[])}.
     *
     * <p>The {@code headerConsumer} is invoked <strong>exactly once</strong>
     * on every code path (success or error); the bytes are a normal
     * wire-format header (length-prefixed JSON).  Use
     * {@link #decodeResponse(byte[])} to extract status / headers /
     * metadata from those bytes.
     */
    public static native void dispatchStreamingWithHeader(
            byte[] wireRequest,
            Consumer<byte[]> headerConsumer,
            OutputStream outputStream);

    /**
     * Variant of {@link #dispatchFullStreaming(byte[], InputStream, OutputStream)}
     * with the same header-callback contract as
     * {@link #dispatchStreamingWithHeader}.  Bidirectional streaming
     * + ability to commit Spring response status/headers before the
     * first body byte.
     */
    public static native void dispatchFullStreamingWithHeader(
            byte[] wireRequestHeader,
            Consumer<byte[]> headerConsumer,
            InputStream inputStream,
            OutputStream outputStream);

    /**
     * Encode a request into the binary wire format.
     *
     * @param method  HTTP method (uppercase: {@code GET}, {@code POST}, ...)
     * @param path    URL path including any path parameters
     * @param query   raw query string (empty / {@code null} if none)
     * @param headers request headers; lowercased keys are recommended
     * @param body    request body bytes (may be empty or {@code null})
     * @return length-prefixed wire bytes ready for {@link #dispatchBytes}
     */
    public static byte[] encodeRequest(
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body) {
        return encodeRequest(null, method, path, query, headers, body);
    }

    /**
     * Encode a request into the binary wire format with an explicit
     * app name for multi-app routing.
     *
     * <p>When {@code appName} is {@code null}, empty, or blank, the
     * request is routed to the <strong>default</strong> app
     * (registered via the Rust {@code register_app} API).  Otherwise
     * the wire header carries {@code "app": "<appName>"} and the
     * request is routed to the named app (registered via
     * {@code register_app_named}).
     *
     * @param appName target app name (may be {@code null} for default)
     * @param method  HTTP method (uppercase: {@code GET}, {@code POST}, ...)
     * @param path    URL path including any path parameters
     * @param query   raw query string (empty / {@code null} if none)
     * @param headers request headers; lowercased keys are recommended
     * @param body    request body bytes (may be empty or {@code null})
     * @return length-prefixed wire bytes ready for any dispatch* method
     */
    public static byte[] encodeRequest(
            String appName,
            String method,
            String path,
            String query,
            Map<String, String> headers,
            byte[] body) {
        try {
            ObjectNode header = MAPPER.createObjectNode();
            header.put("v", WIRE_VERSION);
            header.put("method", method);
            header.put("path", path);
            if (query != null && !query.isEmpty()) {
                header.put("query", query);
            }
            if (headers != null && !headers.isEmpty()) {
                ObjectNode hdrs = MAPPER.createObjectNode();
                for (Map.Entry<String, String> e : headers.entrySet()) {
                    hdrs.put(e.getKey(), e.getValue());
                }
                header.set("headers", hdrs);
            }
            if (appName != null && !appName.isBlank()) {
                header.put("app", appName.trim());
            }
            byte[] headerJson = MAPPER.writeValueAsBytes(header);
            byte[] bodyBytes = body != null ? body : new byte[0];
            ByteBuffer buf = ByteBuffer
                    .allocate(4 + headerJson.length + bodyBytes.length)
                    .order(ByteOrder.BIG_ENDIAN);
            buf.putInt(headerJson.length);
            buf.put(headerJson);
            buf.put(bodyBytes);
            return buf.array();
        } catch (IOException e) {
            throw new IllegalStateException("encodeRequest serialisation failed", e);
        }
    }

    /**
     * Decode a wire-format response.
     *
     * @throws IllegalArgumentException if the wire bytes are malformed
     */
    public static DecodedResponse decodeResponse(byte[] wire) {
        if (wire == null || wire.length < 4) {
            throw new IllegalArgumentException(
                    "wire response too short: "
                            + (wire == null ? "null" : wire.length + " bytes"));
        }
        ByteBuffer buf = ByteBuffer.wrap(wire).order(ByteOrder.BIG_ENDIAN);
        int headerLen = buf.getInt();
        if (headerLen < 0 || (long) 4 + headerLen > wire.length) {
            throw new IllegalArgumentException(
                    "wire header_len " + headerLen
                            + " overflows response (" + wire.length + " bytes)");
        }
        try {
            JsonNode header = MAPPER.readTree(
                    new java.io.ByteArrayInputStream(wire, 4, headerLen));
            int status = header.path("status").asInt(500);

            Map<String, Object> headers = new LinkedHashMap<>();
            JsonNode hdrs = header.path("headers");
            if (hdrs.isObject()) {
                Iterator<Map.Entry<String, JsonNode>> it = hdrs.fields();
                while (it.hasNext()) {
                    Map.Entry<String, JsonNode> e = it.next();
                    JsonNode v = e.getValue();
                    if (v.isArray()) {
                        List<String> list = new ArrayList<>(v.size());
                        for (JsonNode item : v) {
                            list.add(item.asText());
                        }
                        headers.put(e.getKey(), list);
                    } else {
                        headers.put(e.getKey(), v.asText());
                    }
                }
            }

            Map<String, String> metadata = new LinkedHashMap<>();
            JsonNode mdNode = header.path("metadata");
            if (mdNode.isObject()) {
                Iterator<Map.Entry<String, JsonNode>> it = mdNode.fields();
                while (it.hasNext()) {
                    Map.Entry<String, JsonNode> e = it.next();
                    metadata.put(e.getKey(), e.getValue().asText());
                }
            }

            // Hoisted validation errors (Vespera Validated<T> 422 path).
            // null when absent (any non-422 or non-Vespera 422).
            List<Map<String, Object>> validationErrors = null;
            JsonNode veNode = header.path("validation_errors");
            if (veNode.isArray()) {
                validationErrors = new ArrayList<>(veNode.size());
                for (JsonNode item : veNode) {
                    Map<String, Object> entry = new LinkedHashMap<>();
                    Iterator<Map.Entry<String, JsonNode>> it = item.fields();
                    while (it.hasNext()) {
                        Map.Entry<String, JsonNode> e = it.next();
                        entry.put(e.getKey(), e.getValue().asText());
                    }
                    validationErrors.add(entry);
                }
            }

            int bodyStart = 4 + headerLen;
            byte[] body = Arrays.copyOfRange(wire, bodyStart, wire.length);
            return new DecodedResponse(status, headers, metadata, body, validationErrors);
        } catch (IOException e) {
            throw new IllegalArgumentException("wire header JSON parse failed", e);
        }
    }

    // --- Internal: bundled native lib extraction ---

    private static void loadBundled(String libraryName) {
        String os = detectOs();
        String arch = detectArch();
        String filename = mapLibraryName(os, libraryName);
        String resourcePath = "native/" + os + "-" + arch + "/" + filename;

        try (InputStream in =
                VesperaBridge.class.getClassLoader().getResourceAsStream(resourcePath)) {
            if (in == null) {
                throw new UnsatisfiedLinkError("Not found in JAR: " + resourcePath);
            }
            String suffix = filename.substring(filename.lastIndexOf('.'));
            Path temp = Files.createTempFile("vespera-", suffix);
            temp.toFile().deleteOnExit();
            Files.copy(in, temp, StandardCopyOption.REPLACE_EXISTING);
            System.load(temp.toAbsolutePath().toString());
        } catch (IOException e) {
            throw new UnsatisfiedLinkError("Extract failed: " + e.getMessage());
        }
    }

    private static String detectOs() {
        String os = System.getProperty("os.name", "").toLowerCase();
        if (os.contains("win")) return "windows";
        if (os.contains("mac") || os.contains("darwin")) return "macos";
        return "linux";
    }

    private static String detectArch() {
        String arch = System.getProperty("os.arch", "").toLowerCase();
        if (arch.contains("amd64") || arch.contains("x86_64")) return "x86_64";
        if (arch.contains("aarch64") || arch.contains("arm64")) return "aarch64";
        return arch;
    }

    private static String mapLibraryName(String os, String name) {
        return switch (os) {
            case "windows" -> name + ".dll";
            case "macos" -> "lib" + name + ".dylib";
            default -> "lib" + name + ".so";
        };
    }

    private VesperaBridge() {}
}
