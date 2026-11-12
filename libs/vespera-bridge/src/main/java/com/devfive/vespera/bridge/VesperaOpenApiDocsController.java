package com.devfive.vespera.bridge;

import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Objects;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.http.CacheControl;
import org.springframework.http.HttpStatus;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.RestController;

/**
 * {@code vespera.bridge.springdoc.mode=group}: serves each documented Rust
 * app's OpenAPI JSON verbatim at {@code <docs-path>/<app>} (default
 * {@code /v3/api-docs/vespera/_default}); {@link VesperaSwaggerUiUrls} lists
 * these URLs in Swagger UI.  Hidden from springdoc's own document.
 *
 * <ul>
 *   <li>{@code 404} for an app that is not documented (see
 *       {@code vespera.bridge.springdoc.apps});</li>
 *   <li>{@code 503} while the native library is not loaded;</li>
 *   <li>{@code 502} when the Rust spec route fails (e.g. no
 *       {@code spec_url} — the reason is logged).</li>
 * </ul>
 */
@RestController
public class VesperaOpenApiDocsController {

    private static final Logger log = LoggerFactory.getLogger(VesperaOpenApiDocsController.class);

    private final VesperaOpenApiSource source;
    private final List<String> apps;

    public VesperaOpenApiDocsController(VesperaOpenApiSource source, List<String> apps) {
        this.source = Objects.requireNonNull(source, "source");
        this.apps = List.copyOf(apps);
    }

    /** The documented apps, {@code _default} first. */
    public List<String> apps() {
        return apps;
    }

    @GetMapping(value = "${vespera.bridge.springdoc.docs-path:/v3/api-docs/vespera}/{app}",
            produces = MediaType.APPLICATION_JSON_VALUE)
    public ResponseEntity<byte[]> openApi(@PathVariable("app") String app) {
        if (!apps.contains(app)) {
            return error(HttpStatus.NOT_FOUND, "unknown vespera app");
        }
        byte[] json;
        try {
            json = source.openApiJson(app);
        } catch (Exception failure) {
            log.warn("vespera OpenAPI document of app '{}' is unavailable: {}", app, failure.getMessage());
            return error(HttpStatus.BAD_GATEWAY, "vespera OpenAPI document unavailable");
        }
        if (json == null) {
            return error(HttpStatus.SERVICE_UNAVAILABLE, "vespera native library is not initialized");
        }
        return ResponseEntity.ok()
                .contentType(MediaType.APPLICATION_JSON)
                .cacheControl(CacheControl.noCache())
                .body(json);
    }

    private static ResponseEntity<byte[]> error(HttpStatus status, String message) {
        String body = "{\"error\":\"" + message + "\"}";
        return ResponseEntity.status(status)
                .contentType(MediaType.APPLICATION_JSON)
                .body(body.getBytes(StandardCharsets.UTF_8));
    }
}
