package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import org.springframework.http.server.PathContainer;
import org.springframework.web.util.ServletRequestPathUtils;
import org.springframework.web.util.UrlPathHelper;

/**
 * The servlet path-within-application lookup behind
 * {@link ProxyPathPatterns#matches(HttpServletRequest)}. Kept out of
 * {@link ProxyPathPatterns} because these Spring MVC calls widen
 * {@link HttpServletRequest} to {@code ServletRequest}, which makes the
 * bytecode verifier load the servlet API — and {@link ProxyPathPatterns} must
 * also load in a WebFlux application without it.
 *
 * <p>Package-private: an internal collaborator of {@link ProxyPathPatterns}.
 */
final class ServletLookupPath {
    private ServletLookupPath() {}

    /**
     * The path within the application: the one {@code DispatcherServlet}
     * parsed and cached for path-pattern handler mappings, or — when none is
     * cached (e.g. {@code ant_path_matcher} everywhere) — the raw
     * (undecoded) lookup path.
     */
    static PathContainer of(HttpServletRequest request) {
        if (ServletRequestPathUtils.hasParsedRequestPath(request)) {
            return ServletRequestPathUtils.getParsedRequestPath(request).pathWithinApplication();
        }
        return PathContainer.parsePath(UrlPathHelper.rawPathInstance.getLookupPathForRequest(request));
    }
}
