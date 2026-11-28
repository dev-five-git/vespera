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
import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
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
 *   <li><strong>Webhooks</strong> are added by name like paths, without the
 *       proxy patterns (they are not served routes); a name springdoc already
 *       documents keeps its definition (logged).</li>
 *   <li><strong>Components</strong> are added by name; an identical component
 *       is shared, a different one under a clashing name is renamed
 *       {@code <label>_<name>} ({@code label} = {@code vespera} for the default
 *       app, the app name otherwise) and every {@code $ref} to it rewritten.
 *       "Identical" is decided after those renames: a component that only
 *       matches springdoc's by text but refers to a renamed one is renamed
 *       too.</li>
 *   <li>A named app's operations gain a required app-selection header
 *       parameter ({@code vespera.bridge.app-header}, single-value enum) so
 *       "Try it out" reaches that app.</li>
 *   <li>The Rust document's top-level {@code security} is copied onto its
 *       operations that declare none; without one, those operations get an
 *       explicit {@code security: []} when springdoc's document has a
 *       top-level requirement (public Rust routes stay public); its tags are
 *       added when missing.</li>
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
                log.warn("vespera OpenAPI document of app '{}' not merged: {}", app, failure.getMessage(),
                        failure);
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
            resolveNames(mapper, components, existing, label, refRenames);
        }
        if (!refRenames.isEmpty()) {
            rewriteRefs(spec, refRenames);
            renameSecurityRequirements(spec, securitySchemeRenames(refRenames));
        }
        boolean hostSecured = openApi.getSecurity() != null && !openApi.getSecurity().isEmpty();
        prepareOperations(spec, named ? app : null, hostSecured);

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
        mergeWebhooks(openApi, app, vespera.getWebhooks());
        mergeTags(openApi, vespera.getTags());
    }

    /** A Rust component whose name springdoc's document already uses. */
    private static final class Clash {
        final Section section;
        final ObjectNode entries;
        final String name;
        final JsonNode existing;
        String renamedTo;

        Clash(Section section, ObjectNode entries, String name, JsonNode existing) {
            this.section = section;
            this.entries = entries;
            this.name = name;
            this.existing = existing;
        }
    }

    /**
     * Rename the Rust components whose name springdoc already uses for a
     * different component; drop those that are identical (shared).
     *
     * <p>"Identical" is judged on the component <em>after</em> the renames
     * are applied to it, repeated until no new rename appears: a Rust
     * {@code Page} that matches springdoc's {@code Page} textually but whose
     * {@code $ref} to {@code Item} now points at the renamed
     * {@code vespera_Item} is a different component, so it is renamed as
     * well instead of silently adopting springdoc's {@code Item}.
     */
    private static void resolveNames(ObjectMapper mapper, ObjectNode components, Components existing,
                                     String label, Map<String, String> refRenames) {
        List<Clash> clashes = new ArrayList<>();
        for (Section section : SECTIONS) {
            Map<String, ?> current = section.get().apply(existing);
            if (current == null || current.isEmpty()
                    || !(components.get(section.key()) instanceof ObjectNode entries)) {
                continue;
            }
            for (Map.Entry<String, JsonNode> entry : entries.properties()) {
                Object clash = current.get(entry.getKey());
                if (clash != null) {
                    clashes.add(new Clash(section, entries, entry.getKey(), mapper.valueToTree(clash)));
                }
            }
        }
        boolean changed = true;
        while (changed) {
            changed = false;
            for (Clash clash : clashes) {
                if (clash.renamedTo != null) {
                    continue;
                }
                JsonNode candidate = clash.entries.get(clash.name).deepCopy();
                if (!refRenames.isEmpty()) {
                    rewriteRefs(candidate, refRenames);
                }
                if (candidate.equals(clash.existing)) {
                    continue; // identical (so far): reuse springdoc's component
                }
                clash.renamedTo = freeName(clash, clashes, section(existing, clash.section), label);
                refRenames.put(ref(clash.section.key(), clash.name), ref(clash.section.key(), clash.renamedTo));
                changed = true;
            }
        }
        for (Clash clash : clashes) {
            JsonNode value = clash.entries.remove(clash.name);
            if (clash.renamedTo != null) {
                clash.entries.set(clash.renamedTo, value);
            }
        }
    }

    private static Map<String, ?> section(Components components, Section section) {
        return section.get().apply(components);
    }

    /** {@code <label>_<name>}, suffixed {@code _2}, {@code _3}, ... until no name in the section clashes. */
    private static String freeName(Clash clash, List<Clash> clashes, Map<String, ?> existing, String label) {
        String candidate = label + "_" + clash.name;
        for (int n = 2; isTaken(candidate, clash, clashes, existing); n++) {
            candidate = label + "_" + clash.name + "_" + n;
        }
        return candidate;
    }

    private static boolean isTaken(String candidate, Clash clash, List<Clash> clashes, Map<String, ?> existing) {
        if (existing.containsKey(candidate) || clash.entries.has(candidate)) {
            return true;
        }
        for (Clash other : clashes) {
            if (other.section == clash.section && candidate.equals(other.renamedTo)) {
                return true;
            }
        }
        return false;
    }

    private static String ref(String section, String name) {
        return "#/components/" + section + "/" + name.replace("~", "~0").replace("/", "~1");
    }

    /**
     * Rewrite the renamed pointers in place: the value of each {@code $ref} key and
     * each {@code discriminator.mapping} value. Any other string that merely equals
     * a pointer (an example, a default, an enum member, a description) is left alone.
     */
    private static void rewriteRefs(JsonNode node, Map<String, String> renames) {
        rewriteRefs(node, renames, false);
    }

    /**
     * Keys whose value maps names (a schema, a property, a component, a path, ...)
     * to schema-like objects: the keys there are names, not keywords, so a property
     * called {@code example} or {@code default} still gets its {@code $ref}s rewritten.
     */
    private static final Set<String> NAME_MAPS = Set.of("properties", "patternProperties", "dependentSchemas",
            "dependencies", "$defs", "definitions", "schemas", "responses", "parameters", "examples", "requestBodies",
            "headers", "securitySchemes", "links", "callbacks", "pathItems", "paths", "webhooks", "content",
            "encoding");

    /**
     * Keys whose value is data (an example, a default, an enum or const member, an
     * extension): nothing inside them is a pointer. {@code examples} is data only as
     * an array (a JSON Schema list); as an object it is a {@link #NAME_MAPS name map}.
     */
    private static boolean isDataKeyword(String key, JsonNode value) {
        return "example".equals(key) || "default".equals(key) || "enum".equals(key) || "const".equals(key)
                || "value".equals(key) || key.startsWith("x-")
                || ("examples".equals(key) && value.isArray());
    }

    private static void rewriteRefs(JsonNode node, Map<String, String> renames, boolean keysAreNames) {
        if (node instanceof ObjectNode object) {
            for (Map.Entry<String, JsonNode> entry : object.properties()) {
                String key = entry.getKey();
                JsonNode value = entry.getValue();
                if (keysAreNames) {
                    rewriteRefs(value, renames, false);
                } else if (value.isTextual() && "$ref".equals(key)) {
                    entry.setValue(renamedPointer(value, renames));
                } else if ("discriminator".equals(key) && value.get("mapping") instanceof ObjectNode mapping) {
                    for (Map.Entry<String, JsonNode> target : mapping.properties()) {
                        if (target.getValue().isTextual()) {
                            target.setValue(renamedPointer(target.getValue(), renames));
                        }
                    }
                } else if (!isDataKeyword(key, value)) {
                    rewriteRefs(value, renames, NAME_MAPS.contains(key));
                }
            }
        } else if (node instanceof ArrayNode array) {
            for (JsonNode element : array) {
                rewriteRefs(element, renames, false);
            }
        }
    }

    /**
     * The renamed pointer for a textual {@code $ref} value, or the value itself when it was not
     * renamed. The lookup uses the {@link #canonicalPointer canonical} form (Rust percent-encodes
     * the component name, {@code My%20Dto}, while the renames are keyed by the plain name); the
     * rewritten value is encoded the way Rust writes its own {@code $ref}s.
     */
    private static JsonNode renamedPointer(JsonNode pointer, Map<String, String> renames) {
        String target = renames.get(canonicalPointer(pointer.asText()));
        return target == null ? pointer : TextNode.valueOf(encodedPointer(target));
    }

    /**
     * The canonical form of a {@code $ref}: its last segment (the component name) percent-decoded,
     * JSON-pointer-unescaped and escaped again, which is the form {@link #ref} builds. Rust's
     * {@code escape_schema_name} output ({@code My%20Dto}, {@code Pet~1Dog}) therefore maps to the
     * same key as the plain name ({@code My Dto}, {@code Pet/Dog}).
     */
    static String canonicalPointer(String pointer) {
        int slash = pointer.lastIndexOf('/');
        if (slash < 0) {
            return pointer;
        }
        String name = unescape(percentDecode(pointer.substring(slash + 1)));
        return pointer.substring(0, slash + 1) + name.replace("~", "~0").replace("/", "~1");
    }

    /** The inverse of {@link #canonicalPointer} for a pointer built by {@link #ref}: the name segment percent-encoded as Rust does. */
    static String encodedPointer(String pointer) {
        int slash = pointer.lastIndexOf('/');
        StringBuilder out = new StringBuilder(pointer.substring(0, slash + 1));
        for (byte b : pointer.substring(slash + 1).getBytes(StandardCharsets.UTF_8)) {
            int c = b & 0xFF;
            if (isUnreserved(c) || c == '~') {
                out.append((char) c);
            } else {
                out.append('%').append(HEX_DIGITS.charAt(c >> 4)).append(HEX_DIGITS.charAt(c & 0x0F));
            }
        }
        return out.toString();
    }

    private static final String HEX_DIGITS = "0123456789ABCDEF";

    /** {@code [A-Za-z0-9._-]}: the bytes {@code escape_schema_name} leaves as they are. */
    private static boolean isUnreserved(int c) {
        return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')
                || c == '-' || c == '.' || c == '_';
    }

    /** RFC 3986 percent-decoding; a {@code %} not followed by two hex digits is kept as written. */
    static String percentDecode(String text) {
        if (text.indexOf('%') < 0) {
            return text;
        }
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        int i = 0;
        while (i < text.length()) {
            int high = text.charAt(i) == '%' && i + 2 < text.length() ? hexValue(text.charAt(i + 1)) : -1;
            int low = high < 0 ? -1 : hexValue(text.charAt(i + 2));
            if (low >= 0) {
                out.write(high << 4 | low);
                i += 3;
            } else {
                int codePoint = text.codePointAt(i);
                out.writeBytes(new String(Character.toChars(codePoint)).getBytes(StandardCharsets.UTF_8));
                i += Character.charCount(codePoint);
            }
        }
        return out.toString(StandardCharsets.UTF_8);
    }

    private static int hexValue(char c) {
        if (c >= '0' && c <= '9') {
            return c - '0';
        }
        if (c >= 'a' && c <= 'f') {
            return c - 'a' + 10;
        }
        return c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1;
    }

    /** The renamed security schemes ({@code old name → new name}) among {@code refRenames}. */
    static Map<String, String> securitySchemeRenames(Map<String, String> refRenames) {
        String prefix = "#/components/securitySchemes/";
        Map<String, String> names = new HashMap<>();
        refRenames.forEach((from, to) -> {
            if (from.startsWith(prefix) && to.startsWith(prefix)) {
                names.put(unescape(from.substring(prefix.length())), unescape(to.substring(prefix.length())));
            }
        });
        return names;
    }

    private static String unescape(String pointerToken) {
        return pointerToken.replace("~1", "/").replace("~0", "~");
    }

    /**
     * Security requirement objects reference schemes by <em>key</em>
     * ({@code [{"bearer": []}]}), not by {@code $ref}: rename those keys in
     * the document-level {@code security} and in every operation's, including
     * the operations of webhooks and of callbacks (a callback is an
     * expression-to-path-item map, like {@code paths}), and the reusable
     * {@code components.pathItems} and {@code components.callbacks} that the
     * merge copies into the host document.
     */
    private static void renameSecurityRequirements(ObjectNode spec, Map<String, String> renames) {
        if (renames.isEmpty()) {
            return;
        }
        renameRequirementKeys(spec.get("security"), renames);
        renameInPathItems(spec.get("paths"), renames);
        renameInPathItems(spec.get("webhooks"), renames);
        if (spec.get("components") instanceof ObjectNode components) {
            renameInPathItems(components.get("pathItems"), renames);
            renameInCallbacks(components.get("callbacks"), renames);
        }
    }

    /** {@link #renameSecurityRequirements} for the path items of a {@code paths} / {@code webhooks} / callback map. */
    private static void renameInPathItems(JsonNode pathItems, Map<String, String> renames) {
        if (!(pathItems instanceof ObjectNode items)) {
            return;
        }
        for (JsonNode item : items) {
            if (item instanceof ObjectNode pathItem) {
                for (String method : OPERATIONS) {
                    if (pathItem.get(method) instanceof ObjectNode operation) {
                        renameRequirementKeys(operation.get("security"), renames);
                        renameInCallbacks(operation.get("callbacks"), renames);
                    }
                }
            }
        }
    }

    /** {@link #renameSecurityRequirements} for a map of named callbacks (each one an expression-to-path-item map). */
    private static void renameInCallbacks(JsonNode callbacks, Map<String, String> renames) {
        if (callbacks instanceof ObjectNode named) {
            for (JsonNode callback : named) {
                renameInPathItems(callback, renames);
            }
        }
    }

    private static void renameRequirementKeys(JsonNode security, Map<String, String> renames) {
        if (!(security instanceof ArrayNode requirements)) {
            return;
        }
        for (JsonNode requirement : requirements) {
            if (!(requirement instanceof ObjectNode object)) {
                continue;
            }
            boolean touched = false;
            for (String key : renames.keySet()) {
                touched |= object.has(key);
            }
            if (!touched) {
                continue;
            }
            // Rebuild in order so the requirement keeps its key order.
            Map<String, JsonNode> entries = new LinkedHashMap<>();
            object.properties().forEach(entry ->
                    entries.put(renames.getOrDefault(entry.getKey(), entry.getKey()), entry.getValue()));
            object.removeAll();
            entries.forEach(object::set);
        }
    }

    /**
     * Copy the document-level {@code security} onto operations without one
     * (or, when the Rust document has none but {@code hostSecured} — springdoc's
     * document declares a top-level requirement that would otherwise apply
     * to them — set {@code security: []}) and, for a named app, add the
     * app-selection header parameter.
     */
    private void prepareOperations(ObjectNode spec, String namedApp, boolean hostSecured) {
        JsonNode security = spec.get("security");
        boolean copySecurity = security instanceof ArrayNode array && !array.isEmpty();
        boolean markPublic = !copySecurity && hostSecured;
        if (!copySecurity && !markPublic && namedApp == null) {
            return;
        }
        prepareItems(spec.get("paths"), security, copySecurity, markPublic, namedApp);
        // Webhooks are called by the server, never through the proxy: they get
        // the security defaults, not the app-selection header.
        prepareItems(spec.get("webhooks"), security, copySecurity, markPublic, null);
    }

    private void prepareItems(JsonNode items, JsonNode security, boolean copySecurity, boolean markPublic,
                              String namedApp) {
        if (!(items instanceof ObjectNode objects)) {
            return;
        }
        for (JsonNode item : objects) {
            if (!(item instanceof ObjectNode pathItem)) {
                continue;
            }
            for (String method : OPERATIONS) {
                if (!(pathItem.get(method) instanceof ObjectNode operation)) {
                    continue;
                }
                if (copySecurity && !operation.has("security")) {
                    operation.set("security", security.deepCopy());
                } else if (markPublic && !operation.has("security")) {
                    operation.putArray("security");
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

    /**
     * Webhooks (outbound callbacks the app documents, not routes) are merged
     * by name like paths, without the proxy's include / exclude patterns (those
     * match served paths, never a webhook name). A name springdoc already
     * documents keeps its definition.
     */
    private static void mergeWebhooks(OpenAPI openApi, String app, Map<String, PathItem> webhooks) {
        if (webhooks == null || webhooks.isEmpty()) {
            return;
        }
        if (openApi.getWebhooks() == null) {
            openApi.setWebhooks(new LinkedHashMap<>());
        }
        List<String> clashes = new ArrayList<>();
        for (Map.Entry<String, PathItem> entry : webhooks.entrySet()) {
            if (openApi.getWebhooks().containsKey(entry.getKey())) {
                clashes.add(entry.getKey());
                continue;
            }
            openApi.getWebhooks().put(entry.getKey(), entry.getValue());
        }
        if (!clashes.isEmpty()) {
            log.warn("vespera app '{}': webhooks already documented by springdoc were not merged: {}", app, clashes);
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
