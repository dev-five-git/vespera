package com.devfive.vespera.bridge;

import jakarta.servlet.http.HttpServletRequest;
import java.lang.reflect.Method;
import java.security.Principal;
import java.util.ArrayList;
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
 */
public final class ServletPrincipalResolver implements PrincipalResolver {

    private static final String AUTHENTICATION = "org.springframework.security.core.Authentication";
    private static final String GRANTED_AUTHORITY = "org.springframework.security.core.GrantedAuthority";

    /** {@code Authentication#getAuthorities()}, or {@code null} without Spring Security. */
    private final Method getAuthorities;
    /** {@code GrantedAuthority#getAuthority()}, or {@code null} without Spring Security. */
    private final Method getAuthority;

    /** Resolver that detects Spring Security on its own class loader. */
    public ServletPrincipalResolver() {
        this(load(AUTHENTICATION), load(GRANTED_AUTHORITY));
    }

    /**
     * Resolver reading authorities through {@code authenticationType#getAuthorities()}
     * and {@code grantedAuthorityType#getAuthority()}; a {@code null} type (or a
     * missing method) disables authorities.
     */
    ServletPrincipalResolver(Class<?> authenticationType, Class<?> grantedAuthorityType) {
        Method authorities = method(authenticationType, "getAuthorities");
        Method authority = method(grantedAuthorityType, "getAuthority");
        boolean enabled = authorities != null && authority != null;
        this.getAuthorities = enabled ? authorities : null;
        this.getAuthority = enabled ? authority : null;
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
        if (getAuthorities == null || !getAuthorities.getDeclaringClass().isInstance(principal)) {
            return List.of();
        }
        try {
            List<String> out = new ArrayList<>();
            if (getAuthorities.invoke(principal) instanceof Iterable<?> items) {
                for (Object item : items) {
                    if (getAuthority.getDeclaringClass().isInstance(item)
                            && getAuthority.invoke(item) instanceof String value) {
                        out.add(value);
                    }
                }
            }
            return out;
        } catch (ReflectiveOperationException failure) {
            throw new IllegalStateException("could not read the principal's authorities", failure);
        }
    }

    /** The class {@code name} if it is on the classpath, else {@code null}. */
    static Class<?> load(String name) {
        try {
            return Class.forName(name, false, ServletPrincipalResolver.class.getClassLoader());
        } catch (ClassNotFoundException | LinkageError absent) {
            return null;
        }
    }

    /** Public no-arg method {@code name} of {@code type}, or {@code null}. */
    static Method method(Class<?> type, String name) {
        if (type == null) {
            return null;
        }
        try {
            return type.getMethod(name);
        } catch (NoSuchMethodException absent) {
            return null;
        }
    }
}
