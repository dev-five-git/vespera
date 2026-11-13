package com.devfive.vespera.bridge;

import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;

/**
 * Reads Spring Security authorities from an authenticated principal
 * reflectively ({@code Authentication#getAuthorities()} then
 * {@code GrantedAuthority#getAuthority()}), so the bridge keeps no
 * dependency on Spring Security. Shared by {@link ServletPrincipalResolver}
 * and {@link ExchangePrincipalResolver}; servlet- and WebFlux-free.
 *
 * <p>Package-private: an internal collaborator of the principal resolvers.
 */
final class SecurityAuthorities {

    static final String AUTHENTICATION = "org.springframework.security.core.Authentication";
    static final String GRANTED_AUTHORITY = "org.springframework.security.core.GrantedAuthority";
    static final String ANONYMOUS_TOKEN =
            "org.springframework.security.authentication.AnonymousAuthenticationToken";

    /** {@code Authentication#getAuthorities()}, or {@code null} without Spring Security. */
    private final Method getAuthorities;
    /** {@code GrantedAuthority#getAuthority()}, or {@code null} without Spring Security. */
    private final Method getAuthority;

    /** Detects Spring Security on the bridge's own class loader. */
    SecurityAuthorities() {
        this(load(AUTHENTICATION), load(GRANTED_AUTHORITY));
    }

    /**
     * Reads authorities through {@code authenticationType#getAuthorities()}
     * and {@code grantedAuthorityType#getAuthority()}; a {@code null} type (or
     * a missing method) disables authorities.
     */
    SecurityAuthorities(Class<?> authenticationType, Class<?> grantedAuthorityType) {
        Method authorities = method(authenticationType, "getAuthorities");
        Method authority = method(grantedAuthorityType, "getAuthority");
        boolean enabled = authorities != null && authority != null;
        this.getAuthorities = enabled ? authorities : null;
        this.getAuthority = enabled ? authority : null;
    }

    /** Spring Security authorities of {@code principal} ({@code null}s skipped), or none. */
    List<String> of(Object principal) {
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

    /**
     * Whether {@code principal} is Spring Security's anonymous
     * authentication (servlet's {@code getUserPrincipal()} hides it; the
     * reactive exchange principal does not, so the reactive resolver skips it).
     *
     * <p>Compares the names along the principal's superclass chain instead of
     * loading the token class: a lookup that fails (a native image without a
     * reflection hint for it, a class loader that cannot see Spring Security)
     * would otherwise answer {@code false} and forward {@code "anonymousUser"}
     * as an authenticated principal. This check cannot fail open.
     */
    static boolean isAnonymous(Object principal) {
        for (Class<?> type = principal == null ? null : principal.getClass();
                type != null; type = type.getSuperclass()) {
            if (ANONYMOUS_TOKEN.equals(type.getName())) {
                return true;
            }
        }
        return false;
    }

    /** The class {@code name} if it is on the classpath, else {@code null}. */
    static Class<?> load(String name) {
        try {
            return Class.forName(name, false, SecurityAuthorities.class.getClassLoader());
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
