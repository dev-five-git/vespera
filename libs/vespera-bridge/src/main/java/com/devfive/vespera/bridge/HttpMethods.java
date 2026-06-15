package com.devfive.vespera.bridge;

/**
 * Allocation-free HTTP method classification shared by the proxy
 * controller and the dispatch-mode resolvers.
 *
 * <p>Methods are matched case-insensitively via
 * {@link String#equalsIgnoreCase} — which compares in place — instead of
 * allocating an upper-cased copy ({@code method.toUpperCase(Locale.ROOT)})
 * on every request.
 */
final class HttpMethods {

    private HttpMethods() {
    }

    /**
     * Whether {@code method} is idempotent per RFC 9110
     * (GET / HEAD / PUT / DELETE / OPTIONS).  Idempotent requests are
     * safe to re-run, which the DIRECT dispatch path requires for its
     * response-overflow retry.  {@code null} is treated as non-idempotent.
     */
    static boolean isIdempotent(String method) {
        if (method == null) {
            return false;
        }
        return method.equalsIgnoreCase("GET")
                || method.equalsIgnoreCase("HEAD")
                || method.equalsIgnoreCase("PUT")
                || method.equalsIgnoreCase("DELETE")
                || method.equalsIgnoreCase("OPTIONS");
    }
}
