package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;

/**
 * Strategy for the authenticated principal {@link VesperaProxyController}
 * forwards to the Rust handlers (the request wire header's
 * {@code "principal"} field, extracted in Rust as
 * {@code vespera::inprocess::Principal}).
 *
 * <p>The autoconfigured default is {@link ServletPrincipalResolver}
 * ({@link HttpServletRequest#getUserPrincipal()} plus Spring Security
 * authorities when Spring Security is present). Set
 * {@code vespera.bridge.forward-principal=false} to forward none, or
 * register a custom {@code PrincipalResolver} bean (e.g. to add token
 * claims as attributes); the default is then disabled.
 *
 * <p><strong>Security:</strong> the Rust side trusts the resolved
 * principal. Derive it only from authentication state the server
 * established (the container, Spring Security), never from raw
 * client-controlled request headers.
 *
 * <p>Implementations must be safe to call from multiple servlet threads
 * concurrently. WebFlux applications use {@link ReactivePrincipalResolver}.
 */
@FunctionalInterface
public interface PrincipalResolver {

    /**
     * Resolve the principal of {@code request}.
     *
     * @return the principal to forward, or {@code null} for an
     *         unauthenticated request (the field is then omitted)
     */
    VesperaPrincipal resolvePrincipal(HttpServletRequest request);

    /** A resolver that never forwards a principal. */
    static PrincipalResolver none() {
        return request -> null;
    }
}
