package com.devfive.vespera.bridge;

import org.springframework.http.HttpHeaders;
import org.springframework.http.server.reactive.ServerHttpRequest;

import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.atomic.AtomicReference;

/**
 * The request headers + connection facts {@link VesperaReactiveProxyHandler}
 * forwards to Rust — the WebFlux twin of {@link ProxyHeaderSource}: the
 * {@link HeaderPolicy} hop-by-hop / {@code Connection}-nominated filter, minus
 * the app-selection header, the request-id header replaced by the resolved
 * id, repeated headers joined ({@code ", "}, {@code "; "} for {@code cookie}),
 * plus the client's {@code remote_addr} / {@code scheme} and the trusted
 * principal. Servlet-free.
 *
 * <p>Package-private: an internal collaborator of the reactive proxy.
 */
final class ReactiveHeaderSource implements VesperaBridge.HeaderSource {
    private final HttpHeaders headers;
    private final String excludedHeader;
    private final String requestIdHeader;
    private final String requestId;
    private final String remoteAddr;
    private final String scheme;
    private final VesperaPrincipal principal;
    /** Replacement {@code content-length} (a rebuilt form body), or {@code null}. */
    private final String contentLength;

    ReactiveHeaderSource(ServerHttpRequest request, String excludedHeader,
            String requestIdHeader, String requestId, VesperaPrincipal principal) {
        this(request, excludedHeader, requestIdHeader, requestId, principal, null);
    }

    /**
     * @param excludedHeader  lower-case header to drop (the app selector), or {@code null}
     * @param requestIdHeader lower-case request-id header, or {@code null} when disabled
     * @param requestId       id forwarded under {@code requestIdHeader} (replacing the client's)
     * @param principal       trusted principal, or {@code null}
     * @param contentLength   replacement {@code content-length} value, or {@code null}
     */
    ReactiveHeaderSource(ServerHttpRequest request, String excludedHeader,
            String requestIdHeader, String requestId, VesperaPrincipal principal, String contentLength) {
        this.contentLength = contentLength;
        this.headers = request.getHeaders();
        this.excludedHeader = excludedHeader;
        this.requestIdHeader = requestId == null ? null : requestIdHeader;
        this.requestId = requestId;
        this.remoteAddr = remoteAddr(request.getRemoteAddress());
        this.scheme = scheme(request);
        this.principal = principal;
    }

    @Override
    public void writeTo(VesperaBridge.HeaderSink sink) {
        Set<String> connectionTokens = connectionTokens(headers);
        Set<String> seen = new HashSet<>(32);
        headers.forEach((name, values) -> {
            String lower = name.toLowerCase(Locale.ROOT);
            // A multi-map adapter (e.g. Netty's) may list one name in several
            // spellings, each mapping to ALL values: forward it once.
            if (!seen.add(lower)
                    || HeaderPolicy.isHopByHopResponseHeader(lower)
                    || HeaderPolicy.isConnectionNominatedHeader(lower, connectionTokens)
                    || lower.equals(excludedHeader)
                    || lower.equals(requestIdHeader)) {
                return;
            }
            sink.put(lower, contentLength != null && lower.equals("content-length")
                    ? contentLength : join(lower, values));
        });
        if (requestIdHeader != null) {
            sink.put(requestIdHeader, requestId);
        }
    }

    private static Set<String> connectionTokens(HttpHeaders headers) {
        AtomicReference<Set<String>> tokens = new AtomicReference<>();
        headers.forEach((name, values) -> {
            if (name.equalsIgnoreCase("connection") && values != null) {
                for (String value : values) {
                    if (value != null) {
                        tokens.set(HeaderPolicy.addConnectionTokens(tokens.get(), value));
                    }
                }
            }
        });
        return tokens.get();
    }

    /** Joins repeated values per RFC 9110 ({@code "; "} for {@code cookie}, RFC 6265). */
    static String join(String lowerName, List<String> values) {
        if (values == null || values.isEmpty()) {
            return "";
        }
        if (values.size() == 1) {
            String only = values.get(0);
            return only == null ? "" : only;
        }
        String separator = lowerName.equals("cookie") ? "; " : ", ";
        StringBuilder sb = new StringBuilder();
        for (String value : values) {
            if (sb.length() > 0) {
                sb.append(separator);
            }
            sb.append(value == null ? "" : value);
        }
        return sb.toString();
    }

    @Override
    public String remoteAddr() {
        return remoteAddr;
    }

    @Override
    public String scheme() {
        return scheme;
    }

    @Override
    public VesperaPrincipal principal() {
        return principal;
    }

    /**
     * {@code ip:port} of the client (after any {@code ForwardedHeaderTransformer}
     * rewrite), IPv6 bracketed; {@code null} when unknown.
     */
    static String remoteAddr(InetSocketAddress address) {
        if (address == null) {
            return null;
        }
        InetAddress ip = address.getAddress();
        String host = ip != null ? ip.getHostAddress() : address.getHostString();
        if (host == null || host.isEmpty()) {
            return null;
        }
        int zone = host.indexOf('%');
        if (zone >= 0 && host.indexOf(':') >= 0) {
            // IPv6 scope id (fe80::1%eth0) is not part of a socket address literal.
            host = host.substring(0, zone);
        }
        String bracketed = host.indexOf(':') >= 0 && host.charAt(0) != '[' ? "[" + host + "]" : host;
        return bracketed + ":" + Math.max(0, address.getPort());
    }

    /** {@code https} / {@code http} from the request URI (falls back to the TLS session). */
    static String scheme(ServerHttpRequest request) {
        String scheme = request.getURI().getScheme();
        if ("https".equalsIgnoreCase(scheme) || "wss".equalsIgnoreCase(scheme)) {
            return "https";
        }
        if ("http".equalsIgnoreCase(scheme) || "ws".equalsIgnoreCase(scheme)) {
            return "http";
        }
        return request.getSslInfo() != null ? "https" : "http";
    }
}
