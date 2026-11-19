package com.devfive.vespera.bridge;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.util.Collection;
import java.util.LinkedHashSet;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Header-based app selection shared by the servlet
 * ({@link HeaderAppNameResolver}) and reactive
 * ({@link ReactiveHeaderAppNameResolver}) resolvers: trims the raw header
 * value, applies the {@code vespera.bridge.exposed-apps} allowlist and logs
 * the one-time "unrestricted" warning. Servlet- and WebFlux-free.
 *
 * <p>Package-private: an internal collaborator of the resolvers.
 */
final class AppHeaderSelection {

    private static final Logger log = LoggerFactory.getLogger(HeaderAppNameResolver.class);

    private final String headerName;
    /** Allowed app names; {@code null} = property absent, unrestricted (legacy behaviour). */
    private final Set<String> exposedApps;
    private final AtomicBoolean unrestrictedWarned = new AtomicBoolean(false);

    AppHeaderSelection(String headerName, Collection<String> exposedApps) {
        if (headerName == null || headerName.isBlank()) {
            throw new IllegalArgumentException(
                "headerName must not be null or blank");
        }
        this.headerName = headerName;
        this.exposedApps = normalizeAllowlist(exposedApps);
    }

    private static Set<String> normalizeAllowlist(Collection<String> exposedApps) {
        if (exposedApps == null) {
            // Property absent: the only way to stay unrestricted.
            return null;
        }
        Set<String> allowed = new LinkedHashSet<>();
        for (String app : exposedApps) {
            if (app != null && !app.isBlank()) {
                allowed.add(AppNames.trimAscii(app));
            }
        }
        if (allowed.isEmpty()) {
            // A configured list with no real entry (an empty value such as
            // ${ADMIN_APP:} with the variable unset, or only blank entries)
            // must not silently become "unrestricted".
            throw new IllegalArgumentException(
                "vespera.bridge.exposed-apps is set but names no app (empty or blank entries only); "
                    + "remove the property to allow any app, or list the apps to expose");
        }
        allowed.add(AppNameResolver.DEFAULT_APP);
        return Set.copyOf(allowed);
    }

    /** The configured header name (as given). */
    String headerName() {
        return headerName;
    }

    /** The lower-case selection header the proxies strip before forwarding. */
    String lowerHeaderName() {
        return headerName.toLowerCase(Locale.ROOT);
    }

    /** The allowlist (including {@code _default}), or {@code null} when unrestricted. */
    Set<String> exposedApps() {
        return exposedApps;
    }

    /**
     * The app selected by the raw header value ({@code null} when absent or
     * blank = default app).
     *
     * @throws UnknownAppException for a name outside the allowlist
     */
    String resolve(String rawHeaderValue) {
        String app = trimmed(rawHeaderValue);
        if (app == null || AppNameResolver.DEFAULT_APP.equals(app)) {
            return app;
        }
        if (exposedApps == null) {
            if (unrestrictedWarned.compareAndSet(false, true)) {
                // The value is client-controlled: log it only in the bounded,
                // ASCII form Rust accepts, never verbatim.
                log.warn("Request selected vespera app '{}' via the client-controlled {} header "
                        + "and vespera.bridge.exposed-apps is not set: any client can reach any "
                        + "registered app, bypassing path-based security rules. Set "
                        + "vespera.bridge.exposed-apps to the apps this proxy may expose "
                        + "(logged once).", AppNames.loggable(app), headerName);
            }
            return app;
        }
        if (!exposedApps.contains(app)) {
            throw new UnknownAppException(app);
        }
        return app;
    }

    /** ASCII trim (as Rust); a value that is empty after trimming means the default app. */
    private static String trimmed(String value) {
        String trimmed = AppNames.trimAscii(value);
        return trimmed == null || trimmed.isEmpty() ? null : trimmed;
    }
}
