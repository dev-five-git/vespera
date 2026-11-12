package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.Collection;
import java.util.LinkedHashSet;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Default {@link AppNameResolver} — reads the app name from a
 * configurable HTTP request header (typically
 * {@code X-Vespera-App}).
 *
 * <p>When the header is absent (or empty / whitespace), this
 * resolver returns {@code null} so the dispatch layer falls back
 * to the default app registered via the Rust
 * {@code register_app} API.
 *
 * <p><strong>Security:</strong> the header is client-controlled. Without an
 * allowlist any client can route a request into any registered app — e.g.
 * send {@code GET /users} with {@code X-Vespera-App: admin} and reach the
 * {@code admin} app past a path-based Spring Security rule that only guards
 * {@code /admin/**}. Configure {@code vespera.bridge.exposed-apps} (the
 * {@code exposedApps} constructor argument) to restrict selection to the
 * apps that are safe to reach through this proxy; any other name raises
 * {@link UnknownAppException} (a {@code 404} from the proxy, no dispatch).
 * Without an allowlist the first non-default selection logs a one-time
 * {@code WARN}. The selection header itself is never forwarded to Rust
 * ({@link #selectionHeaderName()}).
 */
public final class HeaderAppNameResolver implements AppNameResolver {

    private static final Logger log = LoggerFactory.getLogger(HeaderAppNameResolver.class);

    private final String headerName;
    /** Allowed app names; {@code null} = unrestricted (legacy behaviour). */
    private final Set<String> exposedApps;
    private final AtomicBoolean unrestrictedWarned = new AtomicBoolean(false);

    /**
     * Unrestricted resolver: every non-blank header value is selectable.
     *
     * @param headerName HTTP header to inspect (e.g.
     *                   {@code "X-Vespera-App"})
     */
    public HeaderAppNameResolver(String headerName) {
        this(headerName, null);
    }

    /**
     * @param headerName  HTTP header to inspect (e.g. {@code "X-Vespera-App"})
     * @param exposedApps app names a request may select ({@code _default} is
     *                    always allowed); {@code null} or empty keeps the
     *                    unrestricted behaviour
     */
    public HeaderAppNameResolver(String headerName, Collection<String> exposedApps) {
        if (headerName == null || headerName.isBlank()) {
            throw new IllegalArgumentException(
                "headerName must not be null or blank");
        }
        this.headerName = headerName;
        this.exposedApps = normalizeAllowlist(exposedApps);
    }

    private static Set<String> normalizeAllowlist(Collection<String> exposedApps) {
        if (exposedApps == null) {
            return null;
        }
        Set<String> allowed = new LinkedHashSet<>();
        for (String app : exposedApps) {
            if (app != null && !app.isBlank()) {
                allowed.add(app.strip());
            }
        }
        if (allowed.isEmpty()) {
            return null;
        }
        allowed.add(AppNameResolver.DEFAULT_APP);
        return Set.copyOf(allowed);
    }

    @Override
    public String resolveAppName(HttpServletRequest request) {
        String app = headerValue(request);
        if (app == null || AppNameResolver.DEFAULT_APP.equals(app)) {
            return app;
        }
        if (exposedApps == null) {
            if (unrestrictedWarned.compareAndSet(false, true)) {
                log.warn("Request selected vespera app '{}' via the client-controlled {} header "
                        + "and vespera.bridge.exposed-apps is not set: any client can reach any "
                        + "registered app, bypassing path-based security rules. Set "
                        + "vespera.bridge.exposed-apps to the apps this proxy may expose "
                        + "(logged once).", app, headerName);
            }
            return app;
        }
        if (!exposedApps.contains(app)) {
            throw new UnknownAppException(app);
        }
        return app;
    }

    /** The allowlist (including {@code _default}), or {@code null} when unrestricted. */
    Set<String> exposedApps() {
        return exposedApps;
    }

    /** Whether an allowlist ({@code vespera.bridge.exposed-apps}) is active. */
    boolean isRestricted() {
        return exposedApps != null;
    }

    /** The app-selection header; the proxy strips it before forwarding. */
    @Override
    public String selectionHeaderName() {
        return headerName.toLowerCase(Locale.ROOT);
    }

    private String headerValue(HttpServletRequest request) {
        String value = request.getHeader(headerName);
        if (value == null) {
            return null;
        }
        if (!hasLeadingOrTrailingWhitespace(value)) {
            return value.isEmpty() ? null : value;
        }
        String trimmed = value.strip();
        return trimmed.isEmpty() ? null : trimmed;
    }

    private static boolean hasLeadingOrTrailingWhitespace(String value) {
        int len = value.length();
        return len > 0 && (Character.isWhitespace(value.charAt(0))
                || Character.isWhitespace(value.charAt(len - 1)));
    }
}
