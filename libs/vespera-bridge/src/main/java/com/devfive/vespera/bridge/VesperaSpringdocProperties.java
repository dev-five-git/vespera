package com.devfive.vespera.bridge;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * {@code vespera.bridge.springdoc.*}: shows the Rust routes in the
 * application's springdoc-openapi Swagger UI (see
 * {@link VesperaSpringdocAutoConfiguration}).
 *
 * <p>The Rust app must serve its OpenAPI document, e.g.
 * {@code vespera!(spec_url = "/openapi.json", ...)}: the bridge fetches it
 * in-process (a native {@code GET} dispatch, no HTTP) from {@link #specPath}.
 */
public class VesperaSpringdocProperties {

    /** Integration modes ({@code vespera.bridge.springdoc.mode}). */
    public enum Mode {
        /**
         * Default: every documented app becomes its own entry in the Swagger
         * UI definition dropdown, served verbatim from {@link #docsPath}; the
         * Java controllers keep their springdoc document.
         */
        GROUP,
        /**
         * Merge the Rust paths, components and tags into springdoc's default
         * document (Java paths win on a clash; a clashing component name is
         * prefixed with the app label).
         */
        MERGE
    }

    /**
     * Whether the springdoc integration is enabled (it also needs springdoc
     * on the classpath).  Default {@code true}.
     */
    private boolean enabled = true;

    /**
     * How the Rust OpenAPI document reaches Swagger UI: {@code group}
     * (default — a separate Swagger UI entry per app, document passed through
     * verbatim) or {@code merge} (paths/components merged into springdoc's
     * default document).
     */
    private Mode mode = Mode.GROUP;

    /**
     * Path of the Rust app's OpenAPI JSON route — the {@code spec_url} of its
     * {@code vespera!} invocation.  Fetched in-process with a native
     * {@code GET}.  Default {@code /openapi.json}.
     */
    private String specPath = "/openapi.json";

    /**
     * Per-app overrides of {@link #specPath} (app name to path), for apps
     * whose {@code vespera!} uses another {@code spec_url}.
     */
    private Map<String, String> specPaths = new LinkedHashMap<>();

    /**
     * Apps to document ({@code _default} for the default app).  Default:
     * {@code _default} plus {@code vespera.bridge.exposed-apps}.
     */
    private List<String> apps = new ArrayList<>();

    /**
     * {@code group} mode: base path of the endpoint serving each app's
     * document at {@code <docs-path>/<app>}.  Default
     * {@code /v3/api-docs/vespera} (inside springdoc's own api-docs space, so
     * existing security rules for {@code /v3/api-docs/**} apply).
     */
    private String docsPath = "/v3/api-docs/vespera";

    /**
     * {@code group} mode: Swagger UI label of the default app's entry; a
     * named app is shown as {@code <display-name> (<app>)}.  Default
     * {@code vespera}.
     */
    private String displayName = "vespera";

    public boolean isEnabled() {
        return enabled;
    }

    public void setEnabled(boolean enabled) {
        this.enabled = enabled;
    }

    public Mode getMode() {
        return mode;
    }

    public void setMode(Mode mode) {
        this.mode = mode == null ? Mode.GROUP : mode;
    }

    public String getSpecPath() {
        return specPath;
    }

    public void setSpecPath(String specPath) {
        this.specPath = specPath;
    }

    public Map<String, String> getSpecPaths() {
        return specPaths;
    }

    public void setSpecPaths(Map<String, String> specPaths) {
        this.specPaths = specPaths == null ? new LinkedHashMap<>() : new LinkedHashMap<>(specPaths);
    }

    public List<String> getApps() {
        return apps;
    }

    public void setApps(List<String> apps) {
        this.apps = apps == null ? new ArrayList<>() : new ArrayList<>(apps);
    }

    public String getDocsPath() {
        return docsPath;
    }

    public void setDocsPath(String docsPath) {
        this.docsPath = docsPath;
    }

    public String getDisplayName() {
        return displayName;
    }

    public void setDisplayName(String displayName) {
        this.displayName = displayName;
    }

    /**
     * The documented apps in a stable order, {@code _default} first: {@link #apps}
     * when set, otherwise {@code _default} plus {@code exposedApps}.
     */
    List<String> documentedApps(List<String> exposedApps) {
        Set<String> out = new LinkedHashSet<>();
        List<String> source = apps.isEmpty() ? withDefault(exposedApps) : apps;
        if (source.stream().anyMatch(AppNameResolver.DEFAULT_APP::equals)) {
            out.add(AppNameResolver.DEFAULT_APP);
        }
        for (String app : source) {
            if (app != null && !app.isBlank()) {
                out.add(app.trim());
            }
        }
        return List.copyOf(out);
    }

    private static List<String> withDefault(List<String> exposedApps) {
        List<String> out = new ArrayList<>();
        out.add(AppNameResolver.DEFAULT_APP);
        if (exposedApps != null) {
            out.addAll(exposedApps);
        }
        return out;
    }

    /** The Rust spec route of {@code app}: its {@link #specPaths} entry or {@link #specPath}. */
    String specPathFor(String app) {
        String path = specPaths.get(app);
        if (path == null || path.isBlank()) {
            path = specPath;
        }
        if (path == null || path.isBlank()) {
            path = "/openapi.json";
        }
        path = path.trim();
        return path.startsWith("/") ? path : "/" + path;
    }

    /** {@link #docsPath} without a trailing slash, always starting with {@code /}. */
    String normalizedDocsPath() {
        String path = docsPath == null || docsPath.isBlank() ? "/v3/api-docs/vespera" : docsPath.trim();
        if (!path.startsWith("/")) {
            path = "/" + path;
        }
        while (path.length() > 1 && path.endsWith("/")) {
            path = path.substring(0, path.length() - 1);
        }
        return path;
    }

    /**
     * {@link #normalizedDocsPath()}, rejecting a value the document mapping
     * {@code <docs-path>/{app}} cannot use: the root (it would map
     * {@code /{app}} and shadow the proxy) or a path pattern.
     *
     * @throws IllegalStateException with the property name and the reason
     */
    String validatedDocsPath() {
        String path = normalizedDocsPath();
        if ("/".equals(path)) {
            throw new IllegalStateException("vespera.bridge.springdoc.docs-path='" + docsPath
                    + "' is invalid: the root would map /{app} and shadow the vespera proxy; "
                    + "use a base path such as /v3/api-docs/vespera");
        }
        for (char c : new char[] {'{', '}', '*', '?'}) {
            if (path.indexOf(c) >= 0) {
                throw new IllegalStateException("vespera.bridge.springdoc.docs-path='" + docsPath
                        + "' is invalid: it must be a literal path, not a pattern ('" + c + "')");
            }
        }
        return path;
    }

    /** Swagger UI label of {@code app}. */
    String displayNameFor(String app) {
        String base = displayName == null || displayName.isBlank() ? "vespera" : displayName.trim();
        return AppNameResolver.DEFAULT_APP.equals(app) ? base : base + " (" + app + ")";
    }
}
