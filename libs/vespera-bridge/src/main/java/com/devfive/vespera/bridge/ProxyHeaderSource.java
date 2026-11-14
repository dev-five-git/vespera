package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * The request headers + connection facts {@link VesperaProxyController}
 * forwards to Rust: the {@link HeaderPolicy} filter, minus the app-selection
 * header, with {@code content-length} replaced when the buffered body was
 * rebuilt, the request-id header replaced by the resolved id, and the
 * client's {@code remote_addr} / {@code scheme}, plus the trusted principal.
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class ProxyHeaderSource implements VesperaBridge.HeaderSource {
    private final HttpServletRequest request;
    private final String excludedHeader;
    private final String contentLength;
    private final String requestIdHeader;
    private final String requestId;
    private final String remoteAddr;
    private final String scheme;
    private final VesperaPrincipal principal;

    /**
     * @param excludedHeader  lower-case header to drop (the app selector), or {@code null}
     * @param contentLength   replacement {@code content-length} value, or {@code null}
     * @param requestIdHeader lower-case request-id header, or {@code null} when disabled
     * @param requestId       id forwarded under {@code requestIdHeader} (replacing the client's)
     * @param principal       trusted principal from the {@link PrincipalResolver}, or {@code null}
     */
    ProxyHeaderSource(HttpServletRequest request, String excludedHeader, String contentLength,
            String requestIdHeader, String requestId, VesperaPrincipal principal) {
        this.principal = principal;
        this.request = request;
        this.excludedHeader = excludedHeader;
        this.contentLength = contentLength;
        this.requestIdHeader = requestId == null ? null : requestIdHeader;
        this.requestId = requestId;
        this.remoteAddr = remoteAddr(request);
        this.scheme = scheme(request);
    }

    @Override
    public void writeTo(VesperaBridge.HeaderSink sink) {
        HeaderPolicy.forEachRequestHeader(request, (name, value) -> {
            if (name.equals(excludedHeader) || name.equals(requestIdHeader)) {
                return;
            }
            sink.put(name, contentLength != null && name.equals("content-length") ? contentLength : value);
        });
        if (requestIdHeader != null) {
            sink.put(requestIdHeader, requestId);
        }
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
     * {@code ip:port} as the servlet container reports it (after any
     * {@code RemoteIpValve} / {@code ForwardedHeaderFilter} rewrite), IPv6
     * bracketed; {@code null} when the container reports no address.
     */
    static String remoteAddr(HttpServletRequest request) {
        String addr = request.getRemoteAddr();
        if (addr == null || addr.isEmpty()) {
            return null;
        }
        if (addr.charAt(0) == '[' && addr.endsWith("]")) {
            addr = addr.substring(1, addr.length() - 1);
        }
        int zone = addr.indexOf('%');
        if (zone >= 0 && addr.indexOf(':') >= 0) {
            // IPv6 scope id (fe80::1%eth0) is not part of a socket address
            // literal: Rust would reject the whole value (as the WebFlux
            // source does, see ReactiveHeaderSource#remoteAddr).
            addr = addr.substring(0, zone);
        }
        if (addr.isEmpty()) {
            return null;
        }
        String host = addr.indexOf(':') >= 0 ? "[" + addr + "]" : addr;
        return host + ":" + Math.max(0, request.getRemotePort());
    }

    /** {@code https} / {@code http} (falls back to {@link HttpServletRequest#isSecure()}). */
    static String scheme(HttpServletRequest request) {
        String scheme = request.getScheme();
        if ("https".equalsIgnoreCase(scheme)) {
            return "https";
        }
        if ("http".equalsIgnoreCase(scheme)) {
            return "http";
        }
        return request.isSecure() ? "https" : "http";
    }
}
