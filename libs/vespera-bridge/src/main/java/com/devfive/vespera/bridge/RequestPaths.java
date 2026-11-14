package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletMapping;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.MappingMatch;
import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

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
 *       {@code "/"} mapping leaves the path untouched. The decoded servlet
 *       path is matched against decoded raw segments
 *       ({@link #stripServletPath});</li>
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
        // The context path is not decoded (Servlet spec), so a raw prefix match works.
        path = stripPrefix(path, stripPathParameters(request.getContextPath()));
        String servletPath = prefixMappedServletPath(request);
        if (servletPath != null && !servletPath.isEmpty()) {
            path = stripServletPath(path, servletPath, request.getPathInfo());
        }
        return path.isEmpty() ? "/" : path;
    }

    /**
     * Strip the prefix-mapped servlet path from the raw {@code path}. The
     * servlet path is decoded and normalised by the container ({@code %61pi}
     * → {@code api}, {@code //} merged, dot segments resolved, {@code %2F}
     * possibly decoded) while {@code path} is not, so a raw string compare
     * would miss the prefix and forward the servlet path to Rust. Instead the
     * raw segments are decoded and resolved one by one; the prefix ends after
     * the LAST raw segment at which they equal the servlet path, and the raw
     * remainder after it is forwarded unchanged.
     *
     * <p>When no boundary matches, the container's decoded
     * {@code pathInfo} is re-encoded instead (or, without one, {@code path}
     * is returned as is).
     */
    static String stripServletPath(String path, String servletPath, String pathInfo) {
        String target = resolve(splitDecoded(servletPath));
        ArrayList<String> stack = new ArrayList<>();
        int boundary = -1;
        int i = 0;
        int n = path.length();
        while (i < n) {
            if (path.charAt(i) != '/') {
                // Not an origin-form path; nothing to match against.
                break;
            }
            int end = path.indexOf('/', i + 1);
            if (end < 0) {
                end = n;
            }
            String raw = path.substring(i + 1, end);
            // An empty raw segment (`//`) or one that decodes to slashes only
            // (`%2F`) never moves the boundary: it stays in the remainder.
            List<String> decoded = splitDecoded("/" + raw);
            if (!decoded.isEmpty()) {
                for (String segment : decoded) {
                    push(stack, segment);
                }
                if (target.equals(join(stack))) {
                    boundary = end;
                }
            }
            i = end;
        }
        if (boundary >= 0) {
            return path.substring(boundary);
        }
        if (pathInfo != null) {
            return encodePath(pathInfo);
        }
        return path;
    }

    /** Percent-decode {@code path} and split it into its non-empty segments. */
    private static List<String> splitDecoded(String path) {
        String decoded = percentDecode(path);
        ArrayList<String> out = new ArrayList<>();
        for (String segment : decoded.split("/", -1)) {
            if (!segment.isEmpty()) {
                out.add(segment);
            }
        }
        return out;
    }

    private static String resolve(List<String> segments) {
        ArrayList<String> stack = new ArrayList<>();
        for (String segment : segments) {
            push(stack, segment);
        }
        return join(stack);
    }

    private static void push(List<String> stack, String segment) {
        if (".".equals(segment)) {
            return;
        }
        if ("..".equals(segment)) {
            if (!stack.isEmpty()) {
                stack.remove(stack.size() - 1);
            }
            return;
        }
        stack.add(segment);
    }

    private static String join(List<String> stack) {
        return "/" + String.join("/", stack);
    }

    /** UTF-8 percent-decoding ({@code +} stays literal); a malformed escape is kept as is. */
    static String percentDecode(String s) {
        if (s.indexOf('%') < 0) {
            return s;
        }
        ByteArrayOutputStream out = new ByteArrayOutputStream(s.length());
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '%' && i + 2 < s.length() && hex(s.charAt(i + 1)) >= 0 && hex(s.charAt(i + 2)) >= 0) {
                out.write((hex(s.charAt(i + 1)) << 4) | hex(s.charAt(i + 2)));
                i += 2;
            } else {
                byte[] bytes = String.valueOf(c).getBytes(StandardCharsets.UTF_8);
                if (Character.isHighSurrogate(c) && i + 1 < s.length()) {
                    bytes = s.substring(i, i + 2).getBytes(StandardCharsets.UTF_8);
                    i++;
                }
                out.write(bytes, 0, bytes.length);
            }
        }
        return out.toString(StandardCharsets.UTF_8);
    }

    private static int hex(char c) {
        if (c >= '0' && c <= '9') {
            return c - '0';
        }
        if (c >= 'a' && c <= 'f') {
            return c - 'a' + 10;
        }
        if (c >= 'A' && c <= 'F') {
            return c - 'A' + 10;
        }
        return -1;
    }

    /**
     * Percent-encode a decoded path for the wire: RFC 3986 {@code pchar}s and
     * {@code /} stay, everything else (including {@code %}) is UTF-8 escaped.
     */
    static String encodePath(String decoded) {
        StringBuilder out = new StringBuilder(decoded.length() + 8);
        for (byte b : decoded.getBytes(StandardCharsets.UTF_8)) {
            int c = b & 0xFF;
            if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')
                    || "-._~!$&'()*+,=:@/".indexOf(c) >= 0) {
                out.append((char) c);
            } else {
                out.append('%').append(Character.toUpperCase(Character.forDigit(c >> 4, 16)))
                        .append(Character.toUpperCase(Character.forDigit(c & 0xF, 16)));
            }
        }
        return out.toString();
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

    /** Drop every {@code ;param} suffix from each path segment. */
    static String stripPathParameters(String path) {
        if (path == null || path.indexOf(';') < 0) {
            return path;
        }
        StringBuilder out = new StringBuilder(path.length());
        boolean inParams = false;
        for (int i = 0; i < path.length(); i++) {
            char c = path.charAt(i);
            if (c == '/') {
                inParams = false;
            } else if (c == ';') {
                inParams = true;
            }
            if (!inParams) {
                out.append(c);
            }
        }
        return out.toString();
    }
}
