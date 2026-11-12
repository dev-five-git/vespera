package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletResponse;
import jakarta.servlet.http.HttpServletResponseWrapper;

/**
 * Per-request response wrapper of {@link VesperaProxyController}:
 *
 * <ul>
 *   <li>pre-sets the resolved request id ({@link #applyDefaultRequestId()})
 *       and lets the first request-id header the Rust response carries
 *       <em>replace</em> it instead of duplicating it;</li>
 *   <li>records whether the wire response metadata flagged a caught Rust
 *       panic ({@code "panicked":"true"}).</li>
 * </ul>
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class ProxyServletResponse extends HttpServletResponseWrapper {
    private final String requestIdHeader;
    private final String requestId;
    private boolean defaultApplied;
    private boolean upstreamRequestId;
    private boolean panicked;

    /**
     * @param requestIdHeader header carrying the request id, or {@code null} when disabled
     * @param requestId       resolved request id, or {@code null} when disabled
     */
    ProxyServletResponse(HttpServletResponse response, String requestIdHeader, String requestId) {
        super(response);
        this.requestIdHeader = requestId == null ? null : requestIdHeader;
        this.requestId = requestId;
    }

    /** Echo the resolved request id unless the Rust response sets its own. */
    void applyDefaultRequestId() {
        if (requestIdHeader != null && !upstreamRequestId) {
            super.setHeader(requestIdHeader, requestId);
            defaultApplied = true;
        }
    }

    String requestIdHeader() {
        return requestIdHeader;
    }

    String requestId() {
        return requestId;
    }

    @Override
    public void addHeader(String name, String value) {
        if (claimsRequestId(name)) {
            super.setHeader(name, value);
        } else {
            super.addHeader(name, value);
        }
    }

    @Override
    public void setHeader(String name, String value) {
        claimsRequestId(name);
        super.setHeader(name, value);
    }

    /** The first upstream write of the request-id header replaces the pre-set default. */
    private boolean claimsRequestId(String name) {
        if (requestIdHeader == null || upstreamRequestId || !requestIdHeader.equalsIgnoreCase(name)) {
            return false;
        }
        upstreamRequestId = true;
        return true;
    }

    /** A reset (proxy-written error) drops every header; keep echoing the request id. */
    @Override
    public void reset() {
        super.reset();
        upstreamRequestId = false;
        if (defaultApplied) {
            applyDefaultRequestId();
        }
    }

    void markPanicked() {
        panicked = true;
    }

    boolean panicked() {
        return panicked;
    }

    /** Flag {@code response} as panicked when it is a proxy wrapper. */
    static void markPanicked(HttpServletResponse response) {
        if (response instanceof ProxyServletResponse proxied) {
            proxied.markPanicked();
        }
    }
}
