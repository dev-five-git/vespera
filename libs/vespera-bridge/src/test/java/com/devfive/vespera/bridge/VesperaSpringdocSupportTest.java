package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.stream.Collectors;
import org.junit.jupiter.api.Test;
import org.springdoc.core.properties.AbstractSwaggerUiConfigProperties.SwaggerUrl;
import org.springdoc.core.properties.SpringDocConfigProperties;
import org.springdoc.core.properties.SwaggerUiConfigProperties;
import org.springframework.http.ResponseEntity;

/**
 * Spring-context-free pieces of the springdoc integration: the properties,
 * the in-process {@link VesperaOpenApiSource} (fake native dispatch), the
 * {@code group}-mode docs endpoint and Swagger UI URL registration.
 */
class VesperaSpringdocSupportTest {

    static byte[] responseWire(int status, String body) {
        byte[] header = ("{\"v\":1,\"status\":" + status + ",\"headers\":{\"content-type\":\"application/json\"},"
                + "\"metadata\":{}}").getBytes(StandardCharsets.UTF_8);
        byte[] bytes = body.getBytes(StandardCharsets.UTF_8);
        return ByteBuffer.allocate(4 + header.length + bytes.length)
                .putInt(header.length).put(header).put(bytes).array();
    }

    static String requestHeader(byte[] wire) {
        int length = ByteBuffer.wrap(wire).getInt();
        return new String(wire, 4, length, StandardCharsets.UTF_8);
    }

    // ── properties ────────────────────────────────────────────────────────

    @Test
    void documentedAppsDefaultToTheDefaultAppPlusExposedApps() {
        VesperaSpringdocProperties props = new VesperaSpringdocProperties();
        assertEquals(List.of("_default"), props.documentedApps(List.of()));
        assertEquals(List.of("_default"), props.documentedApps(null));
        assertEquals(List.of("_default", "admin"), props.documentedApps(List.of("admin", " ", "admin")));

        props.setApps(List.of("admin", "_default", " public "));
        assertEquals(List.of("_default", "admin", "public"), props.documentedApps(List.of("other")));
        props.setApps(List.of("admin"));
        assertEquals(List.of("admin"), props.documentedApps(List.of()));
        props.setApps(null);
        assertEquals(List.of("_default"), props.documentedApps(List.of()));
    }

    @Test
    void specPathsDocsPathAndDisplayNamesAreNormalized() {
        VesperaSpringdocProperties props = new VesperaSpringdocProperties();
        assertEquals("/openapi.json", props.specPathFor("_default"));
        props.setSpecPath("docs/spec.json");
        props.setSpecPaths(Map.of("admin", " /admin-openapi.json ", "blank", " "));
        assertEquals("/docs/spec.json", props.specPathFor("_default"));
        assertEquals("/admin-openapi.json", props.specPathFor("admin"));
        assertEquals("/docs/spec.json", props.specPathFor("blank"));
        props.setSpecPath(null);
        props.setSpecPaths(null);
        assertEquals("/openapi.json", props.specPathFor("_default"));

        assertEquals("/v3/api-docs/vespera", props.normalizedDocsPath());
        props.setDocsPath("rust-docs//");
        assertEquals("/rust-docs", props.normalizedDocsPath());
        props.setDocsPath(" ");
        assertEquals("/v3/api-docs/vespera", props.normalizedDocsPath());

        assertEquals("vespera", props.displayNameFor("_default"));
        assertEquals("vespera (admin)", props.displayNameFor("admin"));
        props.setDisplayName("Rust");
        assertEquals("Rust (admin)", props.displayNameFor("admin"));
        props.setDisplayName(null);
        assertEquals("vespera", props.displayNameFor("_default"));

        props.setMode(null);
        assertEquals(VesperaSpringdocProperties.Mode.GROUP, props.getMode());
        assertTrue(props.isEnabled());
        assertTrue(new VesperaBridgeProperties().getSpringdoc().isEnabled());
    }

    @Test
    void proxyPathPatternsMatchOpenApiPathTemplates() {
        assertTrue(ProxyPathPatterns.defaults().matchesPath("/anything/{id}"));
        ProxyPathPatterns patterns = new ProxyPathPatterns(List.of("/api/**"), List.of("/api/internal/**"));
        assertTrue(patterns.matchesPath("/api/users/{id}"));
        assertFalse(patterns.matchesPath("/api/internal/stats"));
        assertFalse(patterns.matchesPath("/health"));
    }

    // ── in-process source ─────────────────────────────────────────────────

    @Test
    void inProcessSourceDispatchesAGetToTheSpecRoute() throws Exception {
        List<String> headers = new ArrayList<>();
        VesperaOpenApiSource source = new InProcessOpenApiSource(
                app -> "admin".equals(app) ? "/admin.json" : "/openapi.json", () -> true, wire -> {
                    headers.add(requestHeader(wire));
                    return responseWire(200, "{\"openapi\":\"3.1.0\"}");
                });

        assertArrayEquals("{\"openapi\":\"3.1.0\"}".getBytes(StandardCharsets.UTF_8),
                source.openApiJson("_default"));
        assertTrue(headers.get(0).contains("\"method\":\"GET\""), headers.get(0));
        assertTrue(headers.get(0).contains("\"path\":\"/openapi.json\""), headers.get(0));
        assertFalse(headers.get(0).contains("\"app\""), headers.get(0));

        source.openApiJson("admin");
        assertTrue(headers.get(1).contains("\"path\":\"/admin.json\""), headers.get(1));
        assertTrue(headers.get(1).contains("\"app\":\"admin\""), headers.get(1));
    }

    @Test
    void inProcessSourceIsUnavailableBeforeInitAndFailsOnNon200() {
        VesperaOpenApiSource notReady = new InProcessOpenApiSource(app -> "/openapi.json", () -> false, wire -> {
            throw new AssertionError("must not dispatch");
        });
        assertNull(assertDoesNotThrowValue(notReady));
        // The public factory checks VesperaBridge's real state: not loaded in unit tests.
        assertNull(assertDoesNotThrowValue(VesperaOpenApiSource.inProcess(app -> "/openapi.json")));

        VesperaOpenApiSource missing = new InProcessOpenApiSource(app -> "/openapi.json", () -> true,
                wire -> responseWire(404, ""));
        IllegalStateException notFound = assertThrows(IllegalStateException.class,
                () -> missing.openApiJson("_default"));
        assertTrue(notFound.getMessage().contains("404"), notFound.getMessage());
        assertTrue(notFound.getMessage().contains("spec_url"), notFound.getMessage());

        VesperaOpenApiSource failing = new InProcessOpenApiSource(app -> "/openapi.json", () -> true,
                wire -> responseWire(500, "x".repeat(500)));
        IllegalStateException serverError = assertThrows(IllegalStateException.class,
                () -> failing.openApiJson("_default"));
        assertTrue(serverError.getMessage().contains("500: xxx"), serverError.getMessage());
        assertTrue(serverError.getMessage().endsWith("..."), serverError.getMessage());
    }

    private static byte[] assertDoesNotThrowValue(VesperaOpenApiSource source) {
        try {
            return source.openApiJson("_default");
        } catch (Exception unexpected) {
            throw new AssertionError(unexpected);
        }
    }

    // ── group mode: docs endpoint ─────────────────────────────────────────

    @Test
    void docsControllerServesOnlyDocumentedAppsVerbatim() {
        VesperaOpenApiDocsController controller = new VesperaOpenApiDocsController(app -> switch (app) {
            case "_default" -> "{\"openapi\":\"3.1.0\"}".getBytes(StandardCharsets.UTF_8);
            case "admin" -> null;
            default -> throw new IllegalStateException("boom");
        }, List.of("_default", "admin", "broken"));
        assertEquals(List.of("_default", "admin", "broken"), controller.apps());

        ResponseEntity<byte[]> ok = controller.openApi("_default");
        assertEquals(200, ok.getStatusCode().value());
        assertEquals("{\"openapi\":\"3.1.0\"}", new String(ok.getBody(), StandardCharsets.UTF_8));
        assertEquals("application/json", ok.getHeaders().getContentType().toString());

        assertEquals(404, controller.openApi("nope").getStatusCode().value());
        assertEquals(503, controller.openApi("admin").getStatusCode().value());
        assertEquals(502, controller.openApi("broken").getStatusCode().value());
    }

    // ── group mode: Swagger UI urls ───────────────────────────────────────

    private static Map<String, SwaggerUrl> byName(Set<SwaggerUrl> urls) {
        return urls.stream().collect(Collectors.toMap(SwaggerUrl::getName, url -> url));
    }

    @Test
    void swaggerUiListsSpringdocsOwnDocumentNextToTheVesperaApps() {
        SwaggerUiConfigProperties ui = new SwaggerUiConfigProperties();
        SpringDocConfigProperties springDoc = new SpringDocConfigProperties();
        VesperaSpringdocProperties props = new VesperaSpringdocProperties();

        VesperaSwaggerUiUrls.register(ui, springDoc, false, props, List.of("_default", "admin"));

        Map<String, SwaggerUrl> urls = byName(ui.getUrls());
        assertEquals(Set.of("application", "vespera", "vespera-admin"), urls.keySet());
        assertEquals(springDoc.getApiDocs().getPath(), urls.get("application").getUrl());
        assertEquals("/v3/api-docs/vespera/_default", urls.get("vespera").getUrl());
        assertEquals("vespera", urls.get("vespera").getDisplayName());
        assertEquals("/v3/api-docs/vespera/admin", urls.get("vespera-admin").getUrl());
        assertEquals("vespera (admin)", urls.get("vespera-admin").getDisplayName());
        assertEquals("application", ui.getUrlsPrimaryName());
    }

    @Test
    void swaggerUiKeepsConfiguredUrlsAndGroups() {
        SpringDocConfigProperties springDoc = new SpringDocConfigProperties();
        VesperaSpringdocProperties props = new VesperaSpringdocProperties();
        props.setDocsPath("/rust-docs/");

        // springdoc groups: springdoc lists them itself, no "application" entry.
        SwaggerUiConfigProperties grouped = new SwaggerUiConfigProperties();
        VesperaSwaggerUiUrls.register(grouped, springDoc, true, props, List.of("_default"));
        assertEquals(Set.of("vespera"), byName(grouped.getUrls()).keySet());
        assertEquals("/rust-docs/_default", byName(grouped.getUrls()).get("vespera").getUrl());
        assertNull(grouped.getUrlsPrimaryName());

        // Explicit urls (incl. one named like ours) are kept untouched.
        SwaggerUiConfigProperties configured = new SwaggerUiConfigProperties();
        Set<SwaggerUrl> own = new LinkedHashSet<>();
        own.add(new SwaggerUrl("mine", "/mine.json", "Mine"));
        own.add(new SwaggerUrl("vespera", "/custom.json", "Custom"));
        configured.setUrls(own);
        VesperaSwaggerUiUrls.register(configured, springDoc, false, props, List.of("_default", "admin"));
        Map<String, SwaggerUrl> urls = byName(configured.getUrls());
        assertEquals(Set.of("mine", "vespera", "vespera-admin"), urls.keySet());
        assertEquals("/custom.json", urls.get("vespera").getUrl());

        // springdoc.swagger-ui.url (single document) becomes the "application" entry.
        SwaggerUiConfigProperties single = new SwaggerUiConfigProperties();
        single.setUrl("/custom-api-docs");
        single.setUrlsPrimaryName("vespera");
        VesperaSwaggerUiUrls.register(single, springDoc, false, props, List.of("_default"));
        assertEquals("/custom-api-docs", byName(single.getUrls()).get("application").getUrl());
        assertEquals("vespera", single.getUrlsPrimaryName());
    }
}
