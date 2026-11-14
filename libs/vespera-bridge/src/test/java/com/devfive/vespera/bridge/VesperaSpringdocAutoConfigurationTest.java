package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import org.junit.jupiter.api.Test;
import org.springframework.boot.SpringApplication;
import org.springframework.boot.SpringBootConfiguration;
import org.springframework.boot.autoconfigure.EnableAutoConfiguration;
import org.springframework.context.ConfigurableApplicationContext;
import org.springframework.context.annotation.Bean;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

/**
 * The springdoc integration in a real Spring Boot application (embedded
 * server, springdoc-openapi-starter-webmvc-ui on the test classpath — 3.x on
 * Boot 4, 2.x in the {@code testBoot3} run) with a fake
 * {@link VesperaOpenApiSource} standing in for the native spec route.
 */
class VesperaSpringdocAutoConfigurationTest {

    static final String DEFAULT_SPEC = "{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"Rust\",\"version\":\"1\"},"
            + "\"paths\":{\"/rust/items\":{\"get\":{\"operationId\":\"listItems\","
            + "\"responses\":{\"200\":{\"description\":\"ok\"}}}}}}";
    static final String ADMIN_SPEC = "{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"Admin\",\"version\":\"1\"},"
            + "\"paths\":{\"/stats\":{\"get\":{\"operationId\":\"adminStats\","
            + "\"responses\":{\"200\":{\"description\":\"ok\"}}}}}}";

    @SpringBootConfiguration
    @EnableAutoConfiguration
    static class App {
        @Bean
        VesperaOpenApiSource fakeVesperaSpec() {
            return app -> ("admin".equals(app) ? ADMIN_SPEC : DEFAULT_SPEC).getBytes(StandardCharsets.UTF_8);
        }

        @Bean
        JavaController javaController() {
            return new JavaController();
        }
    }

    @RestController
    static class JavaController {
        @GetMapping("/java/hello")
        String hello() {
            return "hello";
        }
    }

    private static ConfigurableApplicationContext start(String... properties) {
        String[] args = new String[properties.length + 2];
        args[0] = "--server.port=0";
        args[1] = "--vespera.bridge.exposed-apps=admin";
        System.arraycopy(properties, 0, args, 2, properties.length);
        return new SpringApplication(App.class).run(args);
    }

    private static HttpResponse<String> get(ConfigurableApplicationContext ctx, String path) throws Exception {
        String port = ctx.getEnvironment().getProperty("local.server.port");
        return HttpClient.newHttpClient().send(
                HttpRequest.newBuilder(URI.create("http://127.0.0.1:" + port + path)).build(),
                HttpResponse.BodyHandlers.ofString());
    }

    @Test
    void groupModeListsEachRustAppInSwaggerUi() throws Exception {
        try (ConfigurableApplicationContext ctx = start()) {
            HttpResponse<String> javaDocs = get(ctx, "/v3/api-docs");
            assertEquals(200, javaDocs.statusCode());
            assertTrue(javaDocs.body().contains("/java/hello"), javaDocs.body());
            assertFalse(javaDocs.body().contains("/**"), "proxy catch-all must be hidden: " + javaDocs.body());
            assertFalse(javaDocs.body().contains("/v3/api-docs/vespera"), javaDocs.body());
            assertFalse(javaDocs.body().contains("/rust/items"), javaDocs.body());

            HttpResponse<String> config = get(ctx, "/v3/api-docs/swagger-config");
            assertEquals(200, config.statusCode());
            String body = config.body();
            assertTrue(body.contains("\"url\":\"/v3/api-docs/vespera/_default\""), body);
            assertTrue(body.contains("\"url\":\"/v3/api-docs/vespera/admin\""), body);
            assertTrue(body.contains("\"name\":\"application\""), body);
            assertTrue(body.contains("\"url\":\"/v3/api-docs\""), body);
            assertTrue(body.contains("\"urls.primaryName\":\"application\""), body);

            HttpResponse<String> rust = get(ctx, "/v3/api-docs/vespera/_default");
            assertEquals(200, rust.statusCode());
            assertEquals(DEFAULT_SPEC, rust.body());
            assertEquals(ADMIN_SPEC, get(ctx, "/v3/api-docs/vespera/admin").body());
            assertEquals(404, get(ctx, "/v3/api-docs/vespera/other").statusCode());
        }
    }

    @Test
    void mergeModeAddsTheRustPathsToSpringdocsDocument() throws Exception {
        try (ConfigurableApplicationContext ctx = start("--vespera.bridge.springdoc.mode=merge")) {
            String docs = get(ctx, "/v3/api-docs").body();
            assertTrue(docs.contains("/java/hello"), docs);
            assertTrue(docs.contains("/rust/items"), docs);
            assertTrue(docs.contains("/stats"), docs);
            assertTrue(docs.contains("X-Vespera-App"), docs);
            assertFalse(docs.contains("/**"), docs);
            assertEquals(0, ctx.getBeanNamesForType(VesperaOpenApiDocsController.class).length);
            String config = get(ctx, "/v3/api-docs/swagger-config").body();
            assertFalse(config.contains("vespera"), config);
        }
    }

    @Test
    void swaggerUiIsServedBySpringdocNotProxiedToRust() throws Exception {
        try (ConfigurableApplicationContext ctx = start()) {
            HttpResponse<String> index = get(ctx, "/swagger-ui/index.html");
            assertEquals(200, index.statusCode(), index.body());
            assertTrue(index.headers().firstValue("Content-Type").orElse("").startsWith("text/html"),
                    index.headers().toString());
            assertTrue(index.body().contains("swagger-ui"), index.body());
            assertEquals(200, get(ctx, "/swagger-ui/swagger-ui.css").statusCode());
            // /swagger-ui.html redirects to the index (never reaches Rust).
            assertEquals(302, get(ctx, "/swagger-ui.html").statusCode());
            // Everything else is still proxied (no native library here: 503).
            assertEquals(503, get(ctx, "/rust/items").statusCode());
        }
    }

    @Test
    void customSwaggerUiPathIsExcludedToo() throws Exception {
        try (ConfigurableApplicationContext ctx = start("--springdoc.swagger-ui.path=/docs/ui.html")) {
            assertEquals(200, get(ctx, "/docs/swagger-ui/index.html").statusCode());
            assertEquals(302, get(ctx, "/docs/ui.html").statusCode());
            assertEquals(503, get(ctx, "/swagger-ui/index.html").statusCode(), "the default path is Rust's again");
        }
    }

    @Test
    void docsPathIsMappedFromItsNormalizedFormLikeTheSwaggerUiUrls() throws Exception {
        try (ConfigurableApplicationContext ctx = start("--vespera.bridge.springdoc.docs-path=rust-docs/")) {
            String config = get(ctx, "/v3/api-docs/swagger-config").body();
            assertTrue(config.contains("\"url\":\"/rust-docs/_default\""), config);
            HttpResponse<String> rust = get(ctx, "/rust-docs/_default");
            assertEquals(200, rust.statusCode());
            assertEquals(DEFAULT_SPEC, rust.body());
            assertEquals(ADMIN_SPEC, get(ctx, "/rust-docs/admin").body());
        }
        // Blank = the default path, never a root `/{app}` mapping that would shadow the proxy.
        try (ConfigurableApplicationContext ctx = start("--vespera.bridge.springdoc.docs-path=")) {
            assertEquals(DEFAULT_SPEC, get(ctx, "/v3/api-docs/vespera/_default").body());
            assertEquals(503, get(ctx, "/admin").statusCode(), "still proxied to Rust");
        }
    }

    @Test
    void rootOrPatternDocsPathFailsTheStartupClearly() {
        for (String path : new String[] {"/", "/docs/{x}", "/docs/**"}) {
            Exception failure = org.junit.jupiter.api.Assertions.assertThrows(Exception.class,
                    () -> start("--vespera.bridge.springdoc.docs-path=" + path).close());
            boolean mentioned = false;
            for (Throwable t = failure; t != null; t = t.getCause()) {
                if (String.valueOf(t.getMessage()).contains("vespera.bridge.springdoc.docs-path")) {
                    mentioned = true;
                }
            }
            assertTrue(mentioned, path + ": " + failure);
        }
    }

    @Test
    void disabledIntegrationRegistersNothing() throws Exception {
        try (ConfigurableApplicationContext ctx = start("--vespera.bridge.springdoc.enabled=false")) {
            assertEquals(0, ctx.getBeanNamesForType(VesperaOpenApiDocsController.class).length);
            String docs = get(ctx, "/v3/api-docs").body();
            assertFalse(docs.contains("/rust/items"), docs);
            assertFalse(docs.contains("/**"), docs);
        }
    }
}
