package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import java.lang.reflect.Method;
import java.security.Principal;
import java.util.List;
import java.util.Map;

/**
 * Default {@link PrincipalResolver}: forwards
 * {@link HttpServletRequest#getUserPrincipal()} (the principal the servlet
 * container, or Spring Security's request wrapper, authenticated) with its
 * {@link Principal#getName() name}.
 *
 * <p>When Spring Security is on the classpath and the principal is an
 * {@code org.springframework.security.core.Authentication}, its
 * {@code getAuthorities()} are forwarded as the principal's authorities
 * (each {@code GrantedAuthority#getAuthority()}; {@code null}s skipped).
 * Spring Security is looked up reflectively, so the bridge keeps no
 * dependency on it. Anonymous requests (no user principal) forward nothing.
 *
 * <p>WebFlux applications use {@link ExchangePrincipalResolver}.
 */
public final class ServletPrincipalResolver implements PrincipalResolver {

    private final SecurityAuthorities authorities;

    /** Resolver that detects Spring Security on its own class loader. */
    public ServletPrincipalResolver() {
        this.authorities = new SecurityAuthorities();
    }

    /**
     * Resolver reading authorities through {@code authenticationType#getAuthorities()}
     * and {@code grantedAuthorityType#getAuthority()}; a {@code null} type (or a
     * missing method) disables authorities.
     */
    ServletPrincipalResolver(Class<?> authenticationType, Class<?> grantedAuthorityType) {
        this.authorities = new SecurityAuthorities(authenticationType, grantedAuthorityType);
    }

    @Override
    public VesperaPrincipal resolvePrincipal(HttpServletRequest request) {
        Principal principal = request.getUserPrincipal();
        String name = principal == null ? null : principal.getName();
        if (name == null) {
            return null;
        }
        return new VesperaPrincipal(name, authorities(principal), Map.of());
    }

    /** Spring Security authorities of {@code principal}, or none. */
    List<String> authorities(Principal principal) {
        return authorities.of(principal);
    }

    /** The class {@code name} if it is on the classpath, else {@code null}. */
    static Class<?> load(String name) {
        return SecurityAuthorities.load(name);
    }

    /** Public no-arg method {@code name} of {@code type}, or {@code null}. */
    static Method method(Class<?> type, String name) {
        return SecurityAuthorities.method(type, name);
    }
}
