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
     * (GET / HEAD / PUT / DELETE / OPTIONS). Idempotent requests are not
     * necessarily replay-identical, so this is NOT the DIRECT overflow-retry
     * gate. {@code null} is treated as non-idempotent.
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

    /**
     * Whether {@code method} is "safe" per RFC 9110 §9.2.1
     * (GET / HEAD / OPTIONS) — not intended to mutate server state. Re-running
     * it can still yield a different response (timestamps, random IDs).
     *
     * <p>This is the correct gate for the DIRECT overflow retry, which
     * re-runs the handler: an idempotent-but-unsafe method (PUT / DELETE)
     * can legitimately return a <em>different</em> response on a second run
     * (e.g. a {@code DELETE} returning {@code 204} then {@code 404}), which
     * the retry would wrongly surface to the client. {@code null} is treated
     * as unsafe.
     */
    static boolean isSafe(String method) {
        if (method == null) {
            return false;
        }
        return method.equalsIgnoreCase("GET")
                || method.equalsIgnoreCase("HEAD")
                || method.equalsIgnoreCase("OPTIONS");
    }
}
