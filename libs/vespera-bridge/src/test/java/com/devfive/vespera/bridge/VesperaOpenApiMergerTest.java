package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.swagger.v3.oas.models.Components;
import io.swagger.v3.oas.models.OpenAPI;
import io.swagger.v3.oas.models.Operation;
import io.swagger.v3.oas.models.PathItem;
import io.swagger.v3.oas.models.Paths;
import io.swagger.v3.oas.models.media.Schema;
import io.swagger.v3.oas.models.media.StringSchema;
import io.swagger.v3.oas.models.parameters.Parameter;
import io.swagger.v3.oas.models.tags.Tag;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

/** {@code vespera.bridge.springdoc.mode=merge}: {@link VesperaOpenApiMerger}. */
class VesperaOpenApiMergerTest {

    private static final String DEFAULT_SPEC = """
            {
              "openapi": "3.1.0",
              "info": {"title": "Rust", "version": "1.0.0"},
              "security": [{"bearer": []}],
              "tags": [{"name": "users"}, {"name": "java"}],
              "paths": {
                "/users/{id}": {
                  "get": {
                    "tags": ["users"],
                    "operationId": "getUser",
                    "responses": {"200": {"description": "ok", "content": {"application/json":
                      {"schema": {"$ref": "#/components/schemas/User"}}}}}
                  }
                },
                "/public": {"get": {"security": [], "responses": {"200": {"description": "ok"}}}},
                "/java/hello": {"get": {"responses": {"200": {"description": "rust twin"}}}},
                "/internal/stats": {"get": {"responses": {"200": {"description": "ok"}}}}
              },
              "components": {
                "schemas": {
                  "User": {"type": "object", "properties": {"id": {"type": "integer"},
                    "role": {"$ref": "#/components/schemas/Role"}}},
                  "Role": {"type": "string"},
                  "Shared": {"type": "string"},
                  "Pet": {"oneOf": [{"$ref": "#/components/schemas/User"}],
                    "discriminator": {"propertyName": "kind",
                      "mapping": {"user": "#/components/schemas/User"}}}
                },
                "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}}
              }
            }
            """;

    private static final String ADMIN_SPEC = """
            {
              "openapi": "3.1.0",
              "info": {"title": "Admin", "version": "1.0.0"},
              "paths": {
                "/stats": {"get": {"parameters": [{"name": "q", "in": "query", "schema": {"type": "string"}}],
                  "responses": {"200": {"description": "ok"}}}},
                "/public": {"get": {"responses": {"200": {"description": "admin twin"}}}}
              }
            }
            """;

    /** springdoc's document of the Java side: one path, a clashing {@code User} and an equal {@code Shared}. */
    private static OpenAPI springdocDocument() {
        Schema<Object> javaUser = new Schema<>();
        javaUser.setType("object");
        javaUser.setDescription("the Java user");
        return new OpenAPI()
                .paths(new Paths().addPathItem("/java/hello", new PathItem().get(new Operation().operationId("hello"))))
                .components(new Components()
                        .addSchemas("User", javaUser)
                        .addSchemas("Shared", new StringSchema()))
                .addTagsItem(new Tag().name("java").description("Java tag"));
    }

    private static VesperaOpenApiMerger merger(VesperaOpenApiSource source, List<String> apps) {
        return new VesperaOpenApiMerger(source, apps,
                new ProxyPathPatterns(List.of("/**"), List.of("/internal/**")), "X-App");
    }

    private static byte[] bytes(String json) {
        return json.getBytes(StandardCharsets.UTF_8);
    }

    @Test
    void mergesPathsComponentsAndTagsRenamingClashingComponents() {
        OpenAPI doc = springdocDocument();
        merger(app -> bytes(DEFAULT_SPEC), List.of("_default")).customise(doc);

        // Paths: excluded by the proxy patterns / already documented by springdoc are skipped.
        assertEquals(List.of("/java/hello", "/users/{id}", "/public"), List.copyOf(doc.getPaths().keySet()));
        assertEquals("hello", doc.getPaths().get("/java/hello").getGet().getOperationId());

        // Components: clashing `User` renamed (refs rewritten, incl. discriminator mapping), equal `Shared` shared.
        Map<String, Schema> schemas = doc.getComponents().getSchemas();
        assertEquals("the Java user", schemas.get("User").getDescription());
        assertNotNull(schemas.get("vespera_User"));
        assertNotNull(schemas.get("Role"));
        assertNull(schemas.get("vespera_Shared"));
        assertEquals("#/components/schemas/Role",
                ((Schema<?>) schemas.get("vespera_User").getProperties().get("role")).get$ref());
        Schema<?> pet = schemas.get("Pet");
        assertEquals("#/components/schemas/vespera_User", pet.getOneOf().get(0).get$ref());
        assertEquals("#/components/schemas/vespera_User", pet.getDiscriminator().getMapping().get("user"));
        assertEquals("#/components/schemas/vespera_User", doc.getPaths().get("/users/{id}").getGet()
                .getResponses().get("200").getContent().get("application/json").getSchema().get$ref());
        assertNotNull(doc.getComponents().getSecuritySchemes().get("bearer"));

        // Document-level security copied onto operations without their own.
        assertEquals(List.of("bearer"), List.copyOf(doc.getPaths().get("/users/{id}").getGet()
                .getSecurity().get(0).keySet()));
        assertTrue(doc.getPaths().get("/public").getGet().getSecurity().isEmpty());

        // Tags: missing ones added, existing kept.
        assertEquals(List.of("java", "users"), doc.getTags().stream().map(Tag::getName).toList());
        assertEquals("Java tag", doc.getTags().get(0).getDescription());
    }

    @Test
    void renamedSecuritySchemesAreRenamedInSecurityRequirements() {
        String spec = DEFAULT_SPEC.replace("\"/internal/stats\": {\"get\": {",
                "\"/internal/stats\": {\"get\": {\"security\": [{\"bearer\": [\"read\"], \"apiKey\": []}], ");
        OpenAPI doc = springdocDocument();
        // springdoc already has a *different* `bearer` (a JWT one).
        doc.getComponents().addSecuritySchemes("bearer", new io.swagger.v3.oas.models.security.SecurityScheme()
                .type(io.swagger.v3.oas.models.security.SecurityScheme.Type.HTTP)
                .scheme("bearer").bearerFormat("JWT"));
        // Keep /internal/** so the operation-level requirement is merged too.
        new VesperaOpenApiMerger(app -> bytes(spec), List.of("_default"),
                ProxyPathPatterns.defaults(), "X-App").customise(doc);

        assertEquals("JWT", doc.getComponents().getSecuritySchemes().get("bearer").getBearerFormat());
        assertNotNull(doc.getComponents().getSecuritySchemes().get("vespera_bearer"));
        // Copied top-level requirement: references the renamed key.
        assertEquals(List.of("vespera_bearer"), List.copyOf(doc.getPaths().get("/users/{id}").getGet()
                .getSecurity().get(0).keySet()));
        // Operation-level requirement: renamed key, order and scopes kept, other keys untouched.
        Map<String, List<String>> requirement = doc.getPaths().get("/internal/stats").getGet().getSecurity().get(0);
        assertEquals(List.of("vespera_bearer", "apiKey"), List.copyOf(requirement.keySet()));
        assertEquals(List.of("read"), requirement.get("vespera_bearer"));
        assertTrue(doc.getPaths().get("/public").getGet().getSecurity().isEmpty());
        // springdoc's own operations are untouched.
        assertNull(doc.getPaths().get("/java/hello").getGet().getSecurity());
    }

    @Test
    void securitySchemeRenamesComeFromTheSchemeRefsOnly() {
        assertEquals(Map.of("a/b", "vespera_a/b"), VesperaOpenApiMerger.securitySchemeRenames(Map.of(
                "#/components/securitySchemes/a~1b", "#/components/securitySchemes/vespera_a~1b",
                "#/components/schemas/User", "#/components/schemas/vespera_User")));
    }

    @Test
    void namedAppsGetTheAppHeaderAndLoseClashingPaths() {
        OpenAPI doc = new OpenAPI();
        merger(app -> bytes("_default".equals(app) ? DEFAULT_SPEC : ADMIN_SPEC), List.of("_default", "admin"))
                .customise(doc);

        assertTrue(doc.getPaths().containsKey("/stats"));
        assertEquals("ok", doc.getPaths().get("/public").getGet().getResponses().get("200").getDescription());
        // No springdoc components to clash with: names kept as they are.
        assertNotNull(doc.getComponents().getSchemas().get("User"));

        List<Parameter> parameters = doc.getPaths().get("/stats").getGet().getParameters();
        assertEquals(List.of("q", "X-App"), parameters.stream().map(Parameter::getName).toList());
        Parameter header = parameters.get(1);
        assertEquals("header", header.getIn());
        assertTrue(header.getRequired());
        assertEquals(List.of("admin"), header.getSchema().getEnum());
        // The default app's operations carry no app header.
        assertNull(doc.getPaths().get("/users/{id}").getGet().getParameters());
    }

    @Test
    void repeatedClashesGetNumberedNames() throws Exception {
        OpenAPI doc = new OpenAPI().components(new Components()
                .addSchemas("Role", new Schema<>().description("java"))
                .addSchemas("vespera_Role", new Schema<>().description("java too")));
        merger(app -> null, List.of()).merge(doc, "_default", bytes(DEFAULT_SPEC));
        assertEquals("java", doc.getComponents().getSchemas().get("Role").getDescription());
        assertNotNull(doc.getComponents().getSchemas().get("vespera_Role_2"));
        assertEquals("#/components/schemas/vespera_Role_2",
                ((Schema<?>) doc.getComponents().getSchemas().get("User").getProperties().get("role")).get$ref());
    }

    @Test
    void unavailableOrBrokenDocumentsLeaveSpringdocsDocumentAlone() {
        OpenAPI doc = springdocDocument();
        merger(app -> switch (app) {
            case "_default" -> null;
            case "broken" -> bytes("[1, 2]");
            case "invalid" -> bytes("{not json");
            default -> throw new IllegalStateException("404");
        }, List.of("_default", "broken", "invalid", "missing")).customise(doc);

        assertEquals(List.of("/java/hello"), List.copyOf(doc.getPaths().keySet()));
        assertEquals(2, doc.getComponents().getSchemas().size());
        assertFalse(doc.getTags().isEmpty());

        OpenAPI empty = new OpenAPI();
        merger(app -> bytes("{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"t\",\"version\":\"1\"}}"),
                List.of("_default")).customise(empty);
        assertNull(empty.getPaths());
        assertNull(empty.getComponents());
    }
}
