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
     * Values of filter-set {@code Access-Control-*} headers a Rust value
     * replaced ({@code cors=rust}), restored by {@link #resetProxyOutput()}.
     * Lazily created.
     */
    private Map<String, List<String>> replacedCors;
    /** Guards {@link #asyncClosed} against the ASYNC executor's header writes. */
    private final Object asyncLock = new Object();
    /** Set once the ASYNC request ended (timeout, error, completion). */
    private boolean asyncClosed;

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
            case REPLACE -> {
                rememberReplaced(name);
                super.setHeader(name, value);
            }
            default -> super.addHeader(name, value);
        }
        remember(name, value);
    }

    @Override
    public void setHeader(String name, String value) {
        claimsRequestId(name);
        CorsAction action = corsAction(name);
        if (action == CorsAction.DROP) {
            return;
        }
        if (action == CorsAction.REPLACE) {
            rememberReplaced(name);
        }
        super.setHeader(name, value);
        remember(name, value);
    }

    /** Keep the current (filter-set) values of {@code name} before a Rust value replaces them. */
    private void rememberReplaced(String name) {
        Collection<String> current = super.getHeaders(name);
        if (current == null || current.isEmpty()) {
            return;
        }
        if (replacedCors == null) {
            replacedCors = new LinkedHashMap<>();
        }
        replacedCors.putIfAbsent(name, new ArrayList<>(current));
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
     * ETag / Last-Modified of an ASYNC {@code 200} {@code GET} / {@code HEAD}
     * entity, held back so Spring's {@code HttpEntityMethodProcessor} does not
     * evaluate the request's conditional headers ({@code If-None-Match} →
     * {@code 304}) on a Rust response; {@link VesperaAsyncResponseBodyAdvice}
     * restores them for the entity whose body is {@code body}.
     */
    record DeferredValidators(Object body, HttpHeaders headers) {}

    /** Written by the async response executor, read by the async dispatch thread. */
    private volatile DeferredValidators deferredValidators;

    void deferValidators(Object body, HttpHeaders validators) {
        deferredValidators = new DeferredValidators(body, validators);
    }

    /** The validators deferred for the entity whose body is {@code body} (identity), or {@code null}. */
    HttpHeaders takeDeferredValidators(Object body) {
        DeferredValidators deferred = deferredValidators;
        if (deferred == null || deferred.body() != body) {
            return null;
        }
        deferredValidators = null;
        return deferred.headers();
    }

    /**
     * Apply the same {@code Access-Control-*} de-duplication to the headers
     * of an ASYNC {@code ResponseEntity} (written by Spring after the async
     * dispatch, so the wrapper never sees them) and return the headers to
     * use: with {@code SPRING}, Rust's values for headers this response
     * already carries are left out; with {@code RUST}, they are set on the
     * response directly (replacing Spring's) and left out of the entity.
     * Uses only {@link HttpHeaders} methods present in Spring 6.2 and 7.
     *
     * <p>Runs on the ASYNC response executor: the servlet response is only
     * read and written while the async request is still open
     * ({@link #closeAsync()} not called yet), never after a timeout / error /
     * completion handed it back to the container (which may recycle it).
     */
    HttpHeaders dedupeCorsHeaders(HttpHeaders entityHeaders) {
        if (cors == null) {
            return entityHeaders;
        }
        synchronized (asyncLock) {
            if (asyncClosed) {
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
                            // Same bookkeeping as addHeader/setHeader: a proxy-error
                            // reset restores the filter-set value and drops Rust's.
                            rememberReplaced(name);
                            super.setHeader(name, value);
                            first = false;
                        } else {
                            super.addHeader(name, value);
                        }
                        remember(name, value);
                    }
                }
            });
            return kept;
        }
    }

    /**
     * The ASYNC request ended (timeout, error or completion): from now on the
     * executor-side {@link #dedupeCorsHeaders} leaves the servlet response
     * alone.  Waits for a de-duplication already in progress.
     */
    void closeAsync() {
        synchronized (asyncLock) {
            asyncClosed = true;
        }
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
        if (replacedCors != null) {
            // A filter-set CORS header a Rust value replaced (cors=rust): its
            // Rust value was dropped above, put the filter's back.
            replacedCors.forEach((name, values) -> {
                if (!containsName(foreign, name)) {
                    foreign.put(name, values);
                }
            });
        }
        reset();
        foreign.forEach((name, values) -> values.forEach(value -> super.addHeader(name, value)));
    }

    private static boolean containsName(Map<String, List<String>> headers, String name) {
        for (String key : headers.keySet()) {
            if (key.equalsIgnoreCase(name)) {
                return true;
            }
        }
        return false;
    }

    /** A reset (proxy-written error) drops every header; keep echoing the request id. */
    @Override
    public void reset() {
        super.reset();
        written = null;
        replacedCors = null;
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
