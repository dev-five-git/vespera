package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletResponse;
import jakarta.servlet.http.HttpServletResponseWrapper;
import org.springframework.http.HttpHeaders;

import java.util.ArrayList;
import java.util.Collection;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Per-request response wrapper of {@link VesperaProxyController}:
 *
 * <ul>
 *   <li>pre-sets the resolved request id ({@link #applyDefaultRequestId()})
 *       and lets the first request-id header the Rust response carries
 *       <em>replace</em> it instead of duplicating it;</li>
 *   <li>records whether the wire response metadata flagged a caught Rust
 *       panic ({@code "panicked":"true"});</li>
 *   <li>remembers which headers the proxy wrote, so a proxy error
 *       ({@link #resetProxyOutput()}) drops only those and keeps the headers
 *       servlet filters set before the proxy ran (CORS, security headers,
 *       cookies);</li>
 *   <li>de-duplicates {@code Access-Control-*} response headers against the
 *       ones Spring already set ({@link CorsHandling}): with {@code RUST} the
 *       Rust value replaces Spring's, with {@code SPRING} the Rust value is
 *       dropped when Spring set that header.</li>
 * </ul>
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class ProxyServletResponse extends HttpServletResponseWrapper {
    private static final String CORS_PREFIX = "access-control-";

    private final String requestIdHeader;
    private final String requestId;
    private final CorsHandling cors;
    private boolean defaultApplied;
    private boolean upstreamRequestId;
    private boolean panicked;
    /** Headers the proxy wrote, as alternating name / value entries. Lazily created. */
    private List<String> written;

    /**
     * @param requestIdHeader header carrying the request id, or {@code null} when disabled
     * @param requestId       resolved request id, or {@code null} when disabled
     */
    ProxyServletResponse(HttpServletResponse response, String requestIdHeader, String requestId) {
        this(response, requestIdHeader, requestId, null);
    }

    /** @param cors CORS header de-duplication mode, or {@code null} for none */
    ProxyServletResponse(HttpServletResponse response, String requestIdHeader, String requestId,
            CorsHandling cors) {
        super(response);
        this.requestIdHeader = requestId == null ? null : requestIdHeader;
        this.requestId = requestId;
        this.cors = cors;
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
            return;
        }
        switch (corsAction(name)) {
            case DROP -> {
                return;
            }
            case REPLACE -> super.setHeader(name, value);
            default -> super.addHeader(name, value);
        }
        remember(name, value);
    }

    @Override
    public void setHeader(String name, String value) {
        claimsRequestId(name);
        if (corsAction(name) == CorsAction.DROP) {
            return;
        }
        super.setHeader(name, value);
        remember(name, value);
    }

    private enum CorsAction { ADD, REPLACE, DROP }

    /**
     * What to do with an upstream (Rust) {@code Access-Control-*} header:
     * decided on its first write, by whether the response already carries
     * that header from someone else (Spring's CORS processing or a filter).
     */
    private CorsAction corsAction(String name) {
        if (cors == null || name == null || !name.regionMatches(true, 0, CORS_PREFIX, 0, CORS_PREFIX.length())) {
            return CorsAction.ADD;
        }
        boolean ours = wrote(name);
        if (cors == CorsHandling.SPRING) {
            // Spring owns CORS: keep its header, drop Rust's copy.
            return !ours && super.containsHeader(name) ? CorsAction.DROP : CorsAction.ADD;
        }
        // Rust owns CORS: its first value replaces a Spring/filter-set one.
        return !ours && super.containsHeader(name) ? CorsAction.REPLACE : CorsAction.ADD;
    }

    /**
     * Apply the same {@code Access-Control-*} de-duplication to the headers
     * of an ASYNC {@code ResponseEntity} (written by Spring after the async
     * dispatch, so the wrapper never sees them) and return the headers to
     * use: with {@code SPRING}, Rust's values for headers this response
     * already carries are left out; with {@code RUST}, they are set on the
     * response directly (replacing Spring's) and left out of the entity.
     * Uses only {@link HttpHeaders} methods present in Spring 6.2 and 7.
     */
    HttpHeaders dedupeCorsHeaders(HttpHeaders entityHeaders) {
        if (cors == null) {
            return entityHeaders;
        }
        boolean[] clash = {false};
        entityHeaders.forEach((name, values) -> clash[0] |= isCorsClash(name));
        if (!clash[0]) {
            return entityHeaders;
        }
        HttpHeaders kept = new HttpHeaders();
        entityHeaders.forEach((name, values) -> {
            if (!isCorsClash(name)) {
                kept.addAll(name, values);
            } else if (cors == CorsHandling.RUST) {
                boolean first = true;
                for (String value : values) {
                    if (first) {
                        super.setHeader(name, value);
                        first = false;
                    } else {
                        super.addHeader(name, value);
                    }
                }
            }
        });
        return kept;
    }

    private boolean isCorsClash(String name) {
        return name.regionMatches(true, 0, CORS_PREFIX, 0, CORS_PREFIX.length())
                && super.containsHeader(name);
    }

    private void remember(String name, String value) {
        if (name == null) {
            return;
        }
        if (written == null) {
            written = new ArrayList<>(16);
        }
        written.add(name);
        written.add(value);
    }

    private boolean wrote(String name) {
        if (written == null) {
            return false;
        }
        for (int i = 0, n = written.size(); i < n; i += 2) {
            if (written.get(i).equalsIgnoreCase(name)) {
                return true;
            }
        }
        return false;
    }

    /** The first upstream write of the request-id header replaces the pre-set default. */
    private boolean claimsRequestId(String name) {
        if (requestIdHeader == null || upstreamRequestId || !requestIdHeader.equalsIgnoreCase(name)) {
            return false;
        }
        upstreamRequestId = true;
        return true;
    }

    /**
     * Discard what the proxy wrote, for a proxy-written error: the buffered
     * body always; the headers only when the proxy set any (via a full
     * {@code reset()} that restores every header it did not write).  The
     * request-id echo is re-applied.
     */
    void resetProxyOutput() {
        if (written == null && !upstreamRequestId) {
            resetBuffer();
            return;
        }
        Map<String, List<String>> foreign = new LinkedHashMap<>();
        for (String name : getHeaderNames()) {
            if (name.equalsIgnoreCase("Content-Type")
                    || name.equalsIgnoreCase("Content-Length")
                    || (requestIdHeader != null && requestIdHeader.equalsIgnoreCase(name))) {
                continue;
            }
            Collection<String> current = getHeaders(name);
            List<String> values = current == null ? new ArrayList<>() : new ArrayList<>(current);
            if (written != null) {
                // Drop one occurrence per value the proxy wrote under this name.
                for (int i = 0, n = written.size(); i < n; i += 2) {
                    if (written.get(i).equalsIgnoreCase(name)) {
                        values.remove(written.get(i + 1));
                    }
                }
            }
            if (!values.isEmpty()) {
                foreign.put(name, values);
            }
        }
        reset();
        foreign.forEach((name, values) -> values.forEach(value -> super.addHeader(name, value)));
    }

    /** A reset (proxy-written error) drops every header; keep echoing the request id. */
    @Override
    public void reset() {
        super.reset();
        written = null;
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
