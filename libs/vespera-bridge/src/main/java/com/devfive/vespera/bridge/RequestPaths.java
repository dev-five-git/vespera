package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletMapping;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.MappingMatch;

/**
 * Resolves the path {@link VesperaProxyController} forwards to Rust: the raw
 * (still percent-encoded) request URI relative to the application, i.e.
 * without
 *
 * <ul>
 *   <li>the servlet context path ({@code server.servlet.context-path});</li>
 *   <li>the servlet path when the {@code DispatcherServlet} is mapped by
 *       path prefix ({@code spring.mvc.servlet.path=/api} →
 *       {@code /api/*}, {@link MappingMatch#PATH}) — the default
 *       {@code "/"} mapping leaves the path untouched;</li>
 *   <li>{@code ;}-path parameters such as {@code ;jsessionid=…}, which the
 *       container already consumed and the Rust router would otherwise see
 *       as part of a segment.</li>
 * </ul>
 *
 * <p>Package-private: an internal collaborator of the proxy controller.
 */
final class RequestPaths {
    private RequestPaths() {}

    static String pathWithinApplication(HttpServletRequest request) {
        String path = stripPathParameters(request.getRequestURI());
        path = stripPrefix(path, stripPathParameters(request.getContextPath()));
        path = stripPrefix(path, prefixMappedServletPath(request));
        return path.isEmpty() ? "/" : path;
    }

    /**
     * The servlet path when the current servlet is prefix-mapped
     * ({@code /api/*}), else {@code null}. A container without mapping
     * support is treated as unmapped.
     */
    static String prefixMappedServletPath(HttpServletRequest request) {
        HttpServletMapping mapping;
        try {
            mapping = request.getHttpServletMapping();
        } catch (UnsupportedOperationException unsupported) {
            return null;
        }
        if (mapping == null || mapping.getMappingMatch() != MappingMatch.PATH) {
            return null;
        }
        return request.getServletPath();
    }

    /**
     * Strip {@code prefix} when it is a whole leading path segment of
     * {@code path} (so {@code /apixyz} is never mis-stripped against
     * {@code /api}); {@code null} / empty prefixes leave {@code path} as is.
     */
    static String stripPrefix(String path, String prefix) {
        if (prefix == null || prefix.isEmpty() || !path.startsWith(prefix)) {
            return path;
        }
        if (path.length() > prefix.length() && path.charAt(prefix.length()) != '/') {
            return path;
        }
        return path.substring(prefix.length());
    }

    /** Drop every {@code ;param} suffix from each path segment ({@link PathParameters#strip}). */
    static String stripPathParameters(String path) {
        return PathParameters.strip(path);
    }
}
