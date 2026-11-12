package com.devfive.vespera.bridge;

import org.springframework.web.server.ServerWebExchange;

import java.util.Collection;
import java.util.Set;

/**
 * Default {@link ReactiveAppNameResolver}: the WebFlux twin of
 * {@link HeaderAppNameResolver} — same header, trimming, allowlist
 * ({@code vespera.bridge.exposed-apps}) and one-time warning when no
 * allowlist is configured. See {@link HeaderAppNameResolver} for the
 * security rationale.
 */
public final class ReactiveHeaderAppNameResolver implements ReactiveAppNameResolver {

    private final AppHeaderSelection selection;

    /** Unrestricted resolver: every non-blank header value is selectable. */
    public ReactiveHeaderAppNameResolver(String headerName) {
        this(headerName, null);
    }

    /**
     * @param headerName  HTTP header to inspect (e.g. {@code "X-Vespera-App"})
     * @param exposedApps app names a request may select ({@code _default} is
     *                    always allowed); {@code null} or empty keeps the
     *                    unrestricted behaviour
     */
    public ReactiveHeaderAppNameResolver(String headerName, Collection<String> exposedApps) {
        this.selection = new AppHeaderSelection(headerName, exposedApps);
    }

    @Override
    public String resolveAppName(ServerWebExchange exchange) {
        return selection.resolve(exchange.getRequest().getHeaders().getFirst(selection.headerName()));
    }

    /** The allowlist (including {@code _default}), or {@code null} when unrestricted. */
    Set<String> exposedApps() {
        return selection.exposedApps();
    }

    /** Whether an allowlist ({@code vespera.bridge.exposed-apps}) is active. */
    boolean isRestricted() {
        return selection.exposedApps() != null;
    }

    @Override
    public String selectionHeaderName() {
        return selection.lowerHeaderName();
    }
}
