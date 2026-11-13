package com.devfive.vespera.bridge;

import org.springframework.web.server.ServerWebExchange;
import reactor.core.publisher.Mono;

/**
 * WebFlux counterpart of {@link PrincipalResolver}: the trusted
 * authenticated caller {@link VesperaReactiveProxyHandler} forwards in the
 * request wire header's {@code "principal"} field.
 *
 * <p>The autoconfigured default is {@link ExchangePrincipalResolver}
 * ({@code ServerWebExchange#getPrincipal()}, which Spring Security WebFlux
 * backs with the {@code ReactiveSecurityContextHolder} authentication).
 * Set {@code vespera.bridge.forward-principal=false} to forward none, or
 * register a custom {@code ReactivePrincipalResolver} bean.
 *
 * <p><strong>Security:</strong> the Rust side trusts the resolved principal.
 * Derive it only from server-established authentication state, never from raw
 * client-controlled request headers.
 */
@FunctionalInterface
public interface ReactivePrincipalResolver {

    /**
     * Resolve the principal of {@code exchange}.
     *
     * @return the principal to forward, or an empty {@code Mono} for an
     *         unauthenticated request (the field is then omitted)
     */
    Mono<VesperaPrincipal> resolvePrincipal(ServerWebExchange exchange);

    /** A resolver that never forwards a principal. */
    static ReactivePrincipalResolver none() {
        return exchange -> Mono.empty();
    }
}
