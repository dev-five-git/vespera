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

    /**
     * Maximum number of {@code authorities} and, separately, of {@code attributes}
     * forwarded on the wire. Mirrors the Rust parser's
     * {@code vespera_inprocess::MAX_PRINCIPAL_ENTRIES} (1024), which answers
     * {@code 400} for a longer list; the bridge encoder therefore forwards only
     * the first {@code MAX_ENTRIES} of each (in iteration order) and logs a
     * one-time warning instead of failing every request of such a user.
     */
    public static final int MAX_ENTRIES = 1024;

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
