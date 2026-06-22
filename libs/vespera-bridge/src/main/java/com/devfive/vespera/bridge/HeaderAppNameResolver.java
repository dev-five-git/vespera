package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Default {@link AppNameResolver} — reads the app name from a
 * configurable HTTP request header (typically
 * {@code X-Vespera-App}).
 *
 * <p>When the header is absent (or empty / whitespace), this
 * resolver returns {@code null} so the dispatch layer falls back
 * to the default app registered via the Rust
 * {@code register_app} API.
 */
public final class HeaderAppNameResolver implements AppNameResolver {

    private final String headerName;

    /**
     * @param headerName HTTP header to inspect (e.g.
     *                   {@code "X-Vespera-App"})
     */
    public HeaderAppNameResolver(String headerName) {
        if (headerName == null || headerName.isBlank()) {
            throw new IllegalArgumentException(
                "headerName must not be null or blank");
        }
        this.headerName = headerName;
    }

    @Override
    public String resolveAppName(HttpServletRequest request) {
        String value = request.getHeader(headerName);
        if (value == null) {
            return null;
        }
        String trimmed = value.strip();
        return trimmed.isEmpty() ? null : trimmed;
    }
}
