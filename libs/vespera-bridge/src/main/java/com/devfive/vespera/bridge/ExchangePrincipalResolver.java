package com.devfive.vespera.bridge;

import org.springframework.web.server.ServerWebExchange;
import reactor.core.publisher.Mono;

import java.util.Map;

/**
 * Default {@link ReactivePrincipalResolver}: forwards
 * {@link ServerWebExchange#getPrincipal()} with its name. With Spring
 * Security WebFlux that is the {@code Authentication} of the
 * {@code ReactiveSecurityContextHolder} (its exchange wrapper resolves
 * {@code getPrincipal()} from the security context); its
 * {@code getAuthorities()} are forwarded as the principal's authorities,
 * read reflectively like {@link ServletPrincipalResolver} does, so the bridge
 * keeps no dependency on Spring Security. Anonymous requests (no principal,
 * or Spring Security's {@code AnonymousAuthenticationToken}) forward nothing.
 */
public final class ExchangePrincipalResolver implements ReactivePrincipalResolver {

    private final SecurityAuthorities authorities;

    /** Resolver that detects Spring Security on its own class loader. */
    public ExchangePrincipalResolver() {
        this.authorities = new SecurityAuthorities();
    }

    /** Resolver reading authorities through the given (test) types. */
    ExchangePrincipalResolver(Class<?> authenticationType, Class<?> grantedAuthorityType) {
        this.authorities = new SecurityAuthorities(authenticationType, grantedAuthorityType);
    }

    @Override
    public Mono<VesperaPrincipal> resolvePrincipal(ServerWebExchange exchange) {
        return exchange.getPrincipal().flatMap(principal -> {
            String name = principal.getName();
            return name == null || SecurityAuthorities.isAnonymous(principal)
                    ? Mono.empty()
                    : Mono.just(new VesperaPrincipal(name, authorities.of(principal), Map.of()));
        });
    }
}
