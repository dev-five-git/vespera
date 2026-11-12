package com.devfive.vespera.bridge;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import org.jspecify.annotations.NullMarked;
import org.jspecify.annotations.Nullable;

/**
 * Programmatic, in-process client for the embedded Rust app — call vespera
 * routes from Spring services, scheduled jobs or message listeners without
 * going through HTTP or {@link VesperaProxyController}.
 *
 * <pre>{@code
 * VesperaClient client = VesperaClient.create();          // default app
 * VesperaResponse r = client.get("/users/42")
 *         .header("Accept", "application/json")
 *         .send();
 * r.status(); r.header("content-type"); r.bodyString(); r.validationErrors();
 *
 * client.post("/users").json("{\"name\":\"a\"}").send();
 * client.forApp("admin").get("/dashboard").sendAsync()
 *         .thenAccept(resp -> ...);
 * }</pre>
 *
 * <p>Requests are encoded with the same wire codec the proxy uses
 * ({@link VesperaWireCodec}) and dispatched as follows:
 * <ul>
 *   <li>{@link VesperaRequest#send()} — buffered SYNC
 *       ({@link VesperaBridge#dispatchBytes(byte[])}); on a virtual thread
 *       it parks on {@link VesperaBridge#dispatchAsync} instead of pinning
 *       the carrier ({@link ParkingDispatch}).</li>
 *   <li>{@link VesperaRequest#sendAsync()} — ASYNC
 *       ({@link VesperaBridge#dispatchAsync}); bounded by
 *       {@code vespera.async.maxInFlight} ({@code 503} beyond it).</li>
 *   <li>an {@link InputStream} body — bidirectional streaming
 *       ({@link VesperaBridge#dispatchFullStreaming}): the request body is
 *       pulled chunk-by-chunk, the response is collected in memory.</li>
 * </ul>
 * The pooled DIRECT path stays a proxy optimisation: its per-thread buffer
 * view must be copied out anyway, so the client does not use it.
 *
 * <p>HTTP error statuses are ordinary responses (including the {@code 503}
 * a shut-down runtime answers); only transport failures throw
 * {@link VesperaClientException}. Instances are immutable and thread-safe;
 * the Spring autoconfiguration exposes one as a bean.
 */
@NullMarked
public final class VesperaClient {

    /** Native dispatch seam; package-private so unit tests can fake the JNI side. */
    interface Transport {
        /** Buffered dispatch of a full wire request; returns the wire response. */
        byte[] dispatch(byte[] wireRequest);

        /** Async dispatch; the future always completes with a wire response. */
        CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest);

        /**
         * Bidirectional streaming dispatch of a body-less wire request header;
         * returns the response wire header (the body went to {@code out}).
         */
        byte[] dispatchStreaming(byte[] wireRequestHeader, InputStream in, OutputStream out)
                throws IOException;
    }

    /** The real JNI transport — checks {@link VesperaBridge#isInitialized()} first. */
    static final Transport NATIVE = new Transport() {
        @Override
        public byte[] dispatch(byte[] wireRequest) {
            requireInitialized();
            return ParkingDispatch.dispatchBytes(wireRequest, VesperaBridge.currentThreadIsVirtual());
        }

        @Override
        public CompletableFuture<byte[]> dispatchAsync(byte[] wireRequest) {
            requireInitialized();
            return VesperaBridge.dispatch(wireRequest);
        }

        @Override
        public byte[] dispatchStreaming(byte[] wireRequestHeader, InputStream in, OutputStream out)
                throws IOException {
            requireInitialized();
            return VesperaBridge.dispatchFullStreaming(wireRequestHeader, in, out);
        }

        private void requireInitialized() {
            if (!VesperaBridge.isInitialized()) {
                throw new VesperaClientException(
                        "vespera native library is not initialized: call VesperaBridge.init(...) "
                                + "or set vespera.bridge.library-name");
            }
        }
    };

    private final Transport transport;
    /** Normalised app name; {@code null} = the default app. */
    private final String appName;

    VesperaClient(Transport transport, @Nullable String appName) {
        this.transport = Objects.requireNonNull(transport, "transport");
        this.appName = VesperaWireCodec.normalizedAppName(appName);
    }

    /** A client for the default app (registered with {@code register_app} / {@code jni_app!}). */
    public static VesperaClient create() {
        return new VesperaClient(NATIVE, null);
    }

    /**
     * A client bound to the named app ({@code register_app_named} /
     * {@code jni_apps!}). {@code null}, blank or {@code "_default"} target
     * the default app. An unknown name is answered {@code 404} by Rust.
     */
    public VesperaClient forApp(@Nullable String appName) {
        return new VesperaClient(transport, appName);
    }

    /** The app this client targets, or {@code null} for the default app. */
    public @Nullable String appName() {
        return appName;
    }

    /** Start a {@code GET} request. */
    public VesperaRequest get(String path) {
        return request("GET", path);
    }

    /** Start a {@code HEAD} request. */
    public VesperaRequest head(String path) {
        return request("HEAD", path);
    }

    /** Start a {@code POST} request. */
    public VesperaRequest post(String path) {
        return request("POST", path);
    }

    /** Start a {@code PUT} request. */
    public VesperaRequest put(String path) {
        return request("PUT", path);
    }

    /** Start a {@code PATCH} request. */
    public VesperaRequest patch(String path) {
        return request("PATCH", path);
    }

    /** Start a {@code DELETE} request. */
    public VesperaRequest delete(String path) {
        return request("DELETE", path);
    }

    /** Start an {@code OPTIONS} request. */
    public VesperaRequest options(String path) {
        return request("OPTIONS", path);
    }

    /**
     * Start a request with an arbitrary method token (case-sensitive, sent
     * as given).
     *
     * @param method HTTP method token, e.g. {@code "GET"}
     * @param path   absolute path starting with {@code /}; may carry an
     *               already-encoded {@code ?query}, which {@link
     *               VesperaRequest#query} parameters are appended to
     */
    public VesperaRequest request(String method, String path) {
        return new VesperaRequest(transport, appName, method, path);
    }

    /**
     * Percent-encode one path segment (UTF-8; everything but the RFC 3986
     * unreserved characters), e.g. {@code "/users/" + encodePathSegment(id)}.
     */
    public static String encodePathSegment(String segment) {
        return percentEncode(Objects.requireNonNull(segment, "segment"));
    }

    /** RFC 3986 percent-encoding of UTF-8 bytes; unreserved characters stay literal. */
    static String percentEncode(String s) {
        int n = s.length();
        int i = 0;
        while (i < n && isUnreserved(s.charAt(i))) {
            i++;
        }
        if (i == n) {
            return s;
        }
        StringBuilder sb = new StringBuilder(n + 16).append(s, 0, i);
        byte[] bytes = s.substring(i).getBytes(StandardCharsets.UTF_8);
        for (byte b : bytes) {
            int c = b & 0xFF;
            if (c < 0x80 && isUnreserved((char) c)) {
                sb.append((char) c);
            } else {
                sb.append('%').append(HEX[c >> 4]).append(HEX[c & 0xF]);
            }
        }
        return sb.toString();
    }

    private static final char[] HEX = "0123456789ABCDEF".toCharArray();

    private static boolean isUnreserved(char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')
                || c == '-' || c == '.' || c == '_' || c == '~';
    }

    @Override
    public String toString() {
        return "VesperaClient{app=" + (appName == null ? "_default" : appName) + '}';
    }
}
