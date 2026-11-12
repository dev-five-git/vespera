package com.devfive.vespera.bridge;

import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/**
 * The authenticated caller the bridge forwards to Rust in the request
 * wire header's {@code "principal"} field, where handlers extract it as
 * {@code vespera::inprocess::Principal}.
 *
 * <p>This is a <strong>trusted side channel</strong>: it is produced by a
 * {@link PrincipalResolver} from the servlet container / Spring Security
 * authentication state, never from client-supplied HTTP headers, so a
 * client cannot forge it the way it can forge an {@code X-User} header.
 *
 * @param name        principal name (required, e.g. {@code Principal#getName()})
 * @param authorities granted authorities / roles in order (e.g. {@code ROLE_ADMIN});
 *                    {@code null} means none
 * @param attributes  extra string attributes (tenant, token claims, ...);
 *                    {@code null} means none; iteration order is kept
 */
public record VesperaPrincipal(String name, List<String> authorities, Map<String, String> attributes) {

    /** Validates and defensively copies the components (no {@code null} entries). */
    public VesperaPrincipal {
        Objects.requireNonNull(name, "name");
        authorities = authorities == null ? List.of() : List.copyOf(authorities);
        if (attributes == null || attributes.isEmpty()) {
            attributes = Map.of();
        } else {
            Map<String, String> copy = new LinkedHashMap<>();
            attributes.forEach((key, value) -> copy.put(
                    Objects.requireNonNull(key, "attribute key"),
                    Objects.requireNonNull(value, "attribute value")));
            attributes = Collections.unmodifiableMap(copy);
        }
    }

    /** A principal with {@code name} and {@code authorities} and no attributes. */
    public static VesperaPrincipal of(String name, String... authorities) {
        return new VesperaPrincipal(name, List.of(authorities), Map.of());
    }
}
