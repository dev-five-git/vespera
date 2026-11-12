package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

import java.util.Collection;
import java.util.Set;

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
 *
 * <p>WebFlux applications use {@link ReactiveHeaderAppNameResolver}, which
 * applies the same rules to a {@code ServerWebExchange}.
 */
public final class HeaderAppNameResolver implements AppNameResolver {

    private final AppHeaderSelection selection;

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
        this.selection = new AppHeaderSelection(headerName, exposedApps);
    }

    @Override
    public String resolveAppName(HttpServletRequest request) {
        return selection.resolve(request.getHeader(selection.headerName()));
    }

    /** The allowlist (including {@code _default}), or {@code null} when unrestricted. */
    Set<String> exposedApps() {
        return selection.exposedApps();
    }

    /** Whether an allowlist ({@code vespera.bridge.exposed-apps}) is active. */
    boolean isRestricted() {
        return selection.exposedApps() != null;
    }

    /** The app-selection header; the proxy strips it before forwarding. */
    @Override
    public String selectionHeaderName() {
        return selection.lowerHeaderName();
    }
}
