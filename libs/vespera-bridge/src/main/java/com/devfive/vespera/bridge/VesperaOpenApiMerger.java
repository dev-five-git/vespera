package com.devfive.vespera.bridge;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.fasterxml.jackson.databind.node.TextNode;
import io.swagger.v3.core.util.Json31;
import io.swagger.v3.oas.models.Components;
import io.swagger.v3.oas.models.OpenAPI;
import io.swagger.v3.oas.models.PathItem;
import io.swagger.v3.oas.models.Paths;
import io.swagger.v3.oas.models.tags.Tag;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.Set;
import java.util.function.BiConsumer;
import java.util.function.Function;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springdoc.core.customizers.OpenApiCustomizer;

/**
 * {@code vespera.bridge.springdoc.mode=merge}: merges every documented Rust
 * app's OpenAPI document into springdoc's default document.
 *
 * <ul>
 *   <li><strong>Paths</strong> the proxy does not serve
 *       ({@code vespera.bridge.include-patterns} / {@code exclude-patterns})
 *       are dropped; a path springdoc (or an earlier app) already documents
 *       keeps that definition (logged).</li>
 *   <li><strong>Components</strong> are added by name; an identical component
 *       is shared, a different one under a clashing name is renamed
 *       {@code <label>_<name>} ({@code label} = {@code vespera} for the default
 *       app, the app name otherwise) and every {@code $ref} to it rewritten.</li>
 *   <li>A named app's operations gain a required app-selection header
 *       parameter ({@code vespera.bridge.app-header}, single-value enum) so
 *       "Try it out" reaches that app.</li>
 *   <li>The Rust document's top-level {@code security} is copied onto its
 *       operations that declare none; its tags are added when missing.</li>
 * </ul>
 */
final class VesperaOpenApiMerger implements OpenApiCustomizer {

    private static final Logger log = LoggerFactory.getLogger(VesperaOpenApiMerger.class);

    private static final Set<String> OPERATIONS =
            Set.of("get", "put", "post", "delete", "options", "head", "patch", "trace");

    /** The {@link Components} maps, by their JSON key. */
    private static final List<Section> SECTIONS = List.of(
            section("schemas", Components::getSchemas, Components::setSchemas),
            section("responses", Components::getResponses, Components::setResponses),
            section("parameters", Components::getParameters, Components::setParameters),
            section("examples", Components::getExamples, Components::setExamples),
            section("requestBodies", Components::getRequestBodies, Components::setRequestBodies),
            section("headers", Components::getHeaders, Components::setHeaders),
            section("securitySchemes", Components::getSecuritySchemes, Components::setSecuritySchemes),
            section("links", Components::getLinks, Components::setLinks),
            section("callbacks", Components::getCallbacks, Components::setCallbacks),
            section("pathItems", Components::getPathItems, Components::setPathItems));

    private final VesperaOpenApiSource source;
    private final List<String> apps;
    private final ProxyPathPatterns patterns;
    private final String appHeader;

    VesperaOpenApiMerger(VesperaOpenApiSource source, List<String> apps, ProxyPathPatterns patterns,
                         String appHeader) {
        this.source = Objects.requireNonNull(source, "source");
        this.apps = List.copyOf(apps);
        this.patterns = Objects.requireNonNull(patterns, "patterns");
        this.appHeader = appHeader == null || appHeader.isBlank() ? "X-Vespera-App" : appHeader.trim();
    }

    @Override
    public void customise(OpenAPI openApi) {
        for (String app : apps) {
            try {
                byte[] json = source.openApiJson(app);
                if (json == null) {
                    log.warn("vespera OpenAPI document of app '{}' not merged: the native library is not "
                            + "initialized (call VesperaBridge.init before the first api-docs request)", app);
                    continue;
                }
                merge(openApi, app, json);
            } catch (Exception failure) {
                log.warn("vespera OpenAPI document of app '{}' not merged: {}", app, failure.getMessage());
            }
        }
    }

    /** Merge one app's document ({@code json}) into {@code openApi}. */
    void merge(OpenAPI openApi, String app, byte[] json) throws Exception {
        ObjectMapper mapper = Json31.mapper();
        JsonNode parsed = mapper.readTree(json);
        if (!(parsed instanceof ObjectNode spec)) {
            throw new IllegalArgumentException("not a JSON object");
        }
        boolean named = !AppNameResolver.DEFAULT_APP.equals(app);
        String label = named ? app : "vespera";

        Components existing = openApi.getComponents() == null ? new Components() : openApi.getComponents();
        Map<String, String> refRenames = new HashMap<>();
        if (spec.get("components") instanceof ObjectNode components) {
            for (Section section : SECTIONS) {
                if (components.get(section.key()) instanceof ObjectNode entries) {
                    resolveNames(mapper, section, entries, section.get().apply(existing), label, refRenames);
                }
            }
        }
        if (!refRenames.isEmpty()) {
            rewriteRefs(spec, refRenames);
        }
        prepareOperations(spec, named ? app : null);

        OpenAPI vespera = mapper.treeToValue(spec, OpenAPI.class);
        if (vespera.getComponents() != null) {
            if (openApi.getComponents() == null) {
                openApi.setComponents(existing);
            }
            for (Section section : SECTIONS) {
                section.putAll(existing, section.get().apply(vespera.getComponents()));
            }
        }
        mergePaths(openApi, app, vespera.getPaths());
        mergeTags(openApi, vespera.getTags());
    }

    /**
     * Rename the entries of {@code entries} whose name springdoc already uses
     * for a different component; drop those that are identical (shared).
     */
    private static void resolveNames(ObjectMapper mapper, Section section, ObjectNode entries,
                                     Map<String, ?> existing, String label, Map<String, String> refRenames) {
        if (existing == null || existing.isEmpty()) {
            return;
        }
        Map<String, JsonNode> renamed = new LinkedHashMap<>();
        Iterator<Map.Entry<String, JsonNode>> it = entries.properties().iterator();
        while (it.hasNext()) {
            Map.Entry<String, JsonNode> entry = it.next();
            String name = entry.getKey();
            Object clash = existing.get(name);
            if (clash == null) {
                continue;
            }
            it.remove();
            if (mapper.valueToTree(clash).equals(entry.getValue())) {
                continue; // identical: reuse springdoc's component
            }
            String candidate = label + "_" + name;
            for (int n = 2; existing.containsKey(candidate) || entries.has(candidate)
                    || renamed.containsKey(candidate); n++) {
                candidate = label + "_" + name + "_" + n;
            }
            renamed.put(candidate, entry.getValue());
            refRenames.put(ref(section.key(), name), ref(section.key(), candidate));
        }
        renamed.forEach(entries::set);
    }

    private static String ref(String section, String name) {
        return "#/components/" + section + "/" + name.replace("~", "~0").replace("/", "~1");
    }

    /** Rewrite every string value equal to a renamed {@code $ref} (incl. discriminator mappings). */
    private static void rewriteRefs(JsonNode node, Map<String, String> renames) {
        if (node instanceof ObjectNode object) {
            Iterator<Map.Entry<String, JsonNode>> it = object.properties().iterator();
            while (it.hasNext()) {
                Map.Entry<String, JsonNode> entry = it.next();
                String target = entry.getValue().isTextual() ? renames.get(entry.getValue().asText()) : null;
                if (target != null) {
                    entry.setValue(TextNode.valueOf(target));
                } else {
                    rewriteRefs(entry.getValue(), renames);
                }
            }
        } else if (node instanceof ArrayNode array) {
            for (int i = 0; i < array.size(); i++) {
                JsonNode element = array.get(i);
                String target = element.isTextual() ? renames.get(element.asText()) : null;
                if (target != null) {
                    array.set(i, TextNode.valueOf(target));
                } else {
                    rewriteRefs(element, renames);
                }
            }
        }
    }

    /**
     * Copy the document-level {@code security} onto operations without one and,
     * for a named app, add the app-selection header parameter.
     */
    private void prepareOperations(ObjectNode spec, String namedApp) {
        JsonNode security = spec.get("security");
        boolean copySecurity = security instanceof ArrayNode array && !array.isEmpty();
        if (!(spec.get("paths") instanceof ObjectNode paths) || (!copySecurity && namedApp == null)) {
            return;
        }
        for (JsonNode item : paths) {
            if (!(item instanceof ObjectNode pathItem)) {
                continue;
            }
            for (String method : OPERATIONS) {
                if (!(pathItem.get(method) instanceof ObjectNode operation)) {
                    continue;
                }
                if (copySecurity && !operation.has("security")) {
                    operation.set("security", security.deepCopy());
                }
                if (namedApp != null) {
                    ArrayNode parameters = operation.get("parameters") instanceof ArrayNode list
                            ? list : operation.putArray("parameters");
                    ObjectNode parameter = parameters.addObject();
                    parameter.put("name", appHeader);
                    parameter.put("in", "header");
                    parameter.put("required", true);
                    parameter.put("description", "Selects the vespera app '" + namedApp + "'.");
                    ObjectNode schema = parameter.putObject("schema");
                    schema.put("type", "string");
                    schema.putArray("enum").add(namedApp);
                }
            }
        }
    }

    private void mergePaths(OpenAPI openApi, String app, Paths paths) {
        if (paths == null || paths.isEmpty()) {
            return;
        }
        if (openApi.getPaths() == null) {
            openApi.setPaths(new Paths());
        }
        List<String> clashes = new ArrayList<>();
        for (Map.Entry<String, PathItem> entry : paths.entrySet()) {
            String path = entry.getKey();
            if (!patterns.matchesPath(path)) {
                continue;
            }
            if (openApi.getPaths().containsKey(path)) {
                clashes.add(path);
                continue;
            }
            openApi.getPaths().addPathItem(path, entry.getValue());
        }
        if (!clashes.isEmpty()) {
            log.warn("vespera app '{}': paths already documented by springdoc were not merged: {}", app, clashes);
        }
    }

    private static void mergeTags(OpenAPI openApi, List<Tag> tags) {
        if (tags == null) {
            return;
        }
        for (Tag tag : tags) {
            boolean present = openApi.getTags() != null && openApi.getTags().stream()
                    .anyMatch(existing -> Objects.equals(existing.getName(), tag.getName()));
            if (!present) {
                openApi.addTagsItem(tag);
            }
        }
    }

    private static <T> Section section(String key, Function<Components, Map<String, T>> get,
                                       BiConsumer<Components, Map<String, T>> set) {
        @SuppressWarnings({"unchecked", "rawtypes"})
        Function<Components, Map<String, ?>> getter = (Function) get;
        @SuppressWarnings({"unchecked", "rawtypes"})
        BiConsumer<Components, Map<String, ?>> setter = (BiConsumer) set;
        return new Section(key, getter, setter);
    }

    /** One {@link Components} map. */
    private record Section(String key, Function<Components, Map<String, ?>> get,
                           BiConsumer<Components, Map<String, ?>> set) {

        /** Add {@code entries} not already present in {@code target}. */
        @SuppressWarnings({"unchecked", "rawtypes"})
        void putAll(Components target, Map<String, ?> entries) {
            if (entries == null || entries.isEmpty()) {
                return;
            }
            Map current = get.apply(target);
            if (current == null) {
                current = new LinkedHashMap<>();
                set.accept(target, current);
            }
            for (Map.Entry<String, ?> entry : entries.entrySet()) {
                current.putIfAbsent(entry.getKey(), entry.getValue());
            }
        }
    }
}
