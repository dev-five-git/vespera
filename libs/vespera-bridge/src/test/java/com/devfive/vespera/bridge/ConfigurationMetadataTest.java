package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.io.InputStream;
import org.springframework.core.annotation.MergedAnnotation;
import org.springframework.core.annotation.MergedAnnotations;
import org.springframework.core.type.AnnotationMetadata;
import org.springframework.core.type.MethodMetadata;
import org.springframework.core.type.classreading.MetadataReaderFactory;
import org.springframework.core.type.classreading.SimpleMetadataReaderFactory;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.net.URL;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Enumeration;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;
import java.util.stream.Collectors;
import java.util.stream.Stream;
import org.junit.jupiter.api.Test;
import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;

/**
 * IDE metadata ({@code META-INF/spring-configuration-metadata.json}, generated
 * by the configuration processor and merged with
 * {@code additional-spring-configuration-metadata.json}) documents every
 * property the bridge binds or tests.
 */
class ConfigurationMetadataTest {

    private static final String RESOURCE = "META-INF/spring-configuration-metadata.json";
    private static final String PREFIX = "vespera.bridge";

    /** Nested property groups of {@link VesperaBridgeProperties}. */
    private static final Set<Class<?>> NESTED = Set.of(
            VesperaBridgeProperties.Native.class, VesperaBridgeProperties.RequestId.class,
            VesperaSpringdocProperties.class);

    /** Every class whose {@code @ConditionalOnProperty} reads a property. */
    private static final List<Class<?>> AUTO_CONFIGURATIONS = List.of(
            VesperaBridgeAutoConfiguration.class,
            VesperaBridgeMetricsAutoConfiguration.class,
            VesperaBridgeHealthAutoConfiguration.class,
            VesperaSpringdocAutoConfiguration.class);

    /** Conditions on properties another library owns (and documents). */
    private static final Set<String> FOREIGN_PROPERTIES = Set.of("springdoc.api-docs.enabled");

    private static JsonNode bridgeMetadata() throws Exception {
        Enumeration<URL> resources =
                ConfigurationMetadataTest.class.getClassLoader().getResources(RESOURCE);
        ObjectMapper mapper = new ObjectMapper();
        for (URL url : Collections.list(resources)) {
            try (InputStream in = url.openStream()) {
                JsonNode root = mapper.readTree(in);
                for (JsonNode group : root.path("groups")) {
                    if (PREFIX.equals(group.path("name").asText())) {
                        return root;
                    }
                }
            }
        }
        throw new AssertionError("no " + RESOURCE + " declaring the '" + PREFIX + "' group on the classpath");
    }

    private static Map<String, JsonNode> byName(JsonNode array) {
        Map<String, JsonNode> out = new HashMap<>();
        array.forEach(node -> out.put(node.path("name").asText(), node));
        return out;
    }

    /** The relaxed-binding (kebab-case) property names {@code type} binds under {@code prefix}. */
    private static void boundProperties(Class<?> type, String prefix, Set<String> out) {
        for (Method getter : type.getDeclaredMethods()) {
            if (!Modifier.isPublic(getter.getModifiers()) || Modifier.isStatic(getter.getModifiers())
                    || getter.getParameterCount() != 0) {
                continue;
            }
            String name = getter.getName();
            String bean;
            if (name.startsWith("get") && name.length() > 3) {
                bean = name.substring(3);
            } else if (name.startsWith("is") && name.length() > 2) {
                bean = name.substring(2);
            } else {
                continue;
            }
            String property = prefix + "." + kebab(bean);
            if (NESTED.contains(getter.getReturnType())) {
                boundProperties(getter.getReturnType(), property, out);
                continue;
            }
            boolean hasSetter = Stream.of(type.getDeclaredMethods())
                    .anyMatch(m -> m.getName().equals("set" + bean) && m.getParameterCount() == 1);
            if (hasSetter) {
                out.add(property);
            }
        }
    }

    private static String kebab(String camel) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < camel.length(); i++) {
            char c = camel.charAt(i);
            if (Character.isUpperCase(c) && i > 0) {
                sb.append('-');
            }
            sb.append(Character.toLowerCase(c));
        }
        return sb.toString();
    }

    /**
     * Every {@code @ConditionalOnProperty} key on the autoconfiguration classes,
     * their methods and member classes — read from bytecode (ASM), because the
     * member classes reference optional Boot 3 / Boot 4 health types that are
     * not all loadable on one classpath.
     */
    private static Set<String> conditionalProperties() throws Exception {
        Set<String> out = new TreeSet<>();
        MetadataReaderFactory readers = new SimpleMetadataReaderFactory();
        List<String> pending = new ArrayList<>();
        AUTO_CONFIGURATIONS.forEach(config -> pending.add(config.getName()));
        while (!pending.isEmpty()) {
            AnnotationMetadata metadata = readers.getMetadataReader(pending.remove(0)).getAnnotationMetadata();
            pending.addAll(List.of(metadata.getMemberClassNames()));
            List<MergedAnnotations> annotated = new ArrayList<>();
            annotated.add(metadata.getAnnotations());
            for (MethodMetadata method : metadata.getAnnotatedMethods(ConditionalOnProperty.class.getName())) {
                annotated.add(method.getAnnotations());
            }
            for (MergedAnnotations annotations : annotated) {
                MergedAnnotation<ConditionalOnProperty> condition = annotations.get(ConditionalOnProperty.class);
                if (!condition.isPresent()) {
                    continue;
                }
                String prefix = condition.getString("prefix");
                String dotted = prefix.isEmpty() ? "" : prefix + ".";
                Stream.concat(Stream.of(condition.getStringArray("name")),
                                Stream.of(condition.getStringArray("value")))
                        .forEach(name -> out.add(dotted + name));
            }
        }
        return out;
    }

    @Test
    void everyBoundPropertyIsDocumented() throws Exception {
        Map<String, JsonNode> properties = byName(bridgeMetadata().path("properties"));
        Set<String> bound = new TreeSet<>();
        boundProperties(VesperaBridgeProperties.class, PREFIX, bound);
        assertTrue(bound.size() >= 20, "suspiciously few bound properties: " + bound);
        assertTrue(bound.containsAll(Set.of(
                PREFIX + ".include-patterns", PREFIX + ".exclude-patterns",
                PREFIX + ".request-id.enabled", PREFIX + ".native.dispatch-timeout")), bound.toString());

        for (String name : bound) {
            JsonNode property = properties.get(name);
            assertNotNull(property, "undocumented property " + name);
            assertFalse(property.path("description").asText().isBlank(), "no description for " + name);
        }
        // and nothing stale: every documented vespera.bridge.* key is bound
        Set<String> documented = properties.keySet().stream()
                .filter(name -> name.startsWith(PREFIX + "."))
                .collect(Collectors.toCollection(TreeSet::new));
        assertEquals(bound, documented);
    }

    @Test
    void everyConditionalPropertyIsDocumented() throws Exception {
        Map<String, JsonNode> properties = byName(bridgeMetadata().path("properties"));
        Set<String> conditions = conditionalProperties();
        assertTrue(conditions.containsAll(FOREIGN_PROPERTIES), conditions.toString());
        conditions.removeAll(FOREIGN_PROPERTIES);
        assertTrue(conditions.contains(PREFIX + ".springdoc.mode"), conditions.toString());
        assertTrue(conditions.contains("management.health.vespera.enabled"), conditions.toString());
        for (String name : conditions) {
            JsonNode property = properties.get(name);
            assertNotNull(property, "undocumented @ConditionalOnProperty " + name);
            assertFalse(property.path("description").asText().isBlank(), "no description for " + name);
        }
    }

    @Test
    void enumLikeValuesHaveHints() throws Exception {
        JsonNode root = bridgeMetadata();
        Map<String, JsonNode> hints = byName(root.path("hints"));
        assertEquals(Set.of("smart", "bidirectional-streaming"),
                hintValues(hints.get(PREFIX + ".dispatch-mode")));
        assertEquals(Stream.of(CorsHandling.values())
                        .map(c -> c.name().toLowerCase(Locale.ROOT)).collect(Collectors.toSet()),
                hintValues(hints.get(PREFIX + ".cors")));
        assertEquals(Stream.of(VesperaSpringdocProperties.Mode.values())
                        .map(m -> m.name().toLowerCase(Locale.ROOT)).collect(Collectors.toSet()),
                hintValues(hints.get(PREFIX + ".springdoc.mode")));

        Map<String, JsonNode> properties = byName(root.path("properties"));
        assertEquals("smart", properties.get(PREFIX + ".dispatch-mode").path("defaultValue").asText());
        assertEquals("/**", properties.get(PREFIX + ".include-patterns").path("defaultValue").path(0).asText());
        assertEquals(VesperaProxyController.DEFAULT_MAX_BUFFERED_REQUEST_BYTES,
                properties.get(PREFIX + ".max-buffered-request-bytes").path("defaultValue").asLong());
        assertEquals("group", properties.get(PREFIX + ".springdoc.mode").path("defaultValue").asText());
        assertFalse(byName(root.path("groups")).get(PREFIX + ".springdoc")
                .path("description").asText().isBlank());
        JsonNode forwardPrincipal = properties.get(PREFIX + ".forward-principal");
        assertTrue(forwardPrincipal.path("defaultValue").asBoolean(), "forward-principal defaults to true");
        assertTrue(forwardPrincipal.path("description").asText().contains("never"),
                "forward-principal documents that the principal never comes from client headers");
        assertFalse(byName(root.path("groups")).get(PREFIX + ".native")
                .path("description").asText().isBlank());
    }

    private static Set<String> hintValues(JsonNode hint) {
        assertNotNull(hint);
        Set<String> values = new TreeSet<>();
        hint.path("values").forEach(value -> {
            assertFalse(value.path("description").asText().isBlank());
            values.add(value.path("value").asText());
        });
        return values;
    }
}
