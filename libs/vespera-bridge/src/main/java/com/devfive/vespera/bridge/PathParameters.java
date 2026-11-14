package com.devfive.vespera.bridge;

/**
 * Removes {@code ;}-path parameters (matrix variables, {@code ;jsessionid=…})
 * from a raw request path, shared by the servlet ({@link RequestPaths}) and
 * WebFlux ({@link VesperaReactiveProxyHandler}) proxies so both forward the
 * same path to Rust: the Rust router would otherwise see the parameters as
 * part of a segment ({@code /users/42;jsessionid=x} → {@code 404} / {@code 400}).
 *
 * <p>Servlet-free (the WebFlux proxy runs without the servlet API).
 * Package-private: an internal collaborator of the proxies.
 */
final class PathParameters {
    private PathParameters() {}

    /** Drop every {@code ;param} suffix from each path segment ({@code null} stays {@code null}). */
    static String strip(String path) {
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
