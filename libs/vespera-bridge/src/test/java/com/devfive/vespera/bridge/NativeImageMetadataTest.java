package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.fail;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import java.util.stream.Stream;
import org.junit.jupiter.api.Test;
import org.springframework.aot.hint.ExecutableHint;
import org.springframework.aot.hint.ExecutableMode;
import org.springframework.aot.hint.RuntimeHints;
import org.springframework.aot.hint.TypeHint;
import org.springframework.aot.hint.TypeReference;
import org.springframework.context.annotation.ImportRuntimeHints;

/**
 * Keeps the GraalVM native-image reachability metadata shipped in the JAR
 * ({@code META-INF/native-image/kr.devfive/vespera-bridge/}) honest.
 *
 * <p>A native image only answers JNI {@code FindClass} / {@code GetMethodID} /
 * {@code ThrowNew} for what that metadata registers, and the miss surfaces only
 * at run time inside the native image (the streaming method cache falls back,
 * {@code ThrowNew} fails silently). So this test reads the Rust sources of
 * {@code crates/vespera_jni} and requires every JNI lookup there to be
 * registered: a new lookup without metadata fails here, in the ordinary
 * {@code ./gradlew test}, instead of in somebody's native build.
 */
class NativeImageMetadataTest {

    private static final String METADATA_DIR = "META-INF/native-image/kr.devfive/vespera-bridge/";

    /** {@code crates/vespera_jni/src}, relative to this Gradle project. */
    private static final Path JNI_SOURCES = Paths.get("..", "..", "crates", "vespera_jni", "src");

    /**
     * The {@code jni} crate version the internal lookups below were audited
     * against. Its own bindings ({@code bind_java_type!}) resolve these classes
     * and their methods by name — e.g. {@code Env::throw_new} looks up
     * {@code Throwable} through the thread context class loader — so a bump of
     * the dependency needs a re-audit (run the demo-app E2E suite under
     * {@code -agentlib:native-image-agent} and diff its {@code jni-config.json}).
     */
    private static final String AUDITED_JNI_CRATE = "0.22";

    /**
     * Every type the {@code jni} crate binds ({@code bind_java_type!} in
     * {@code src/objects} and {@code src/exceptions.rs}). Its bindings resolve
     * ALL their methods on first use and its exception matching
     * ({@code jni_try!}) looks classes up through those same bindings, so one
     * unregistered member makes the lookup fail inside its own error handling
     * and recurse until the request thread's stack overflows (seen in a native
     * image as a {@code StackOverflowError} in {@code JNIFunctions.ExceptionCheck}
     * on the second bidirectional upload). Registered with all public members.
     */
    private static final Set<String> JNI_CRATE_CLASSES = Set.of(
            "java.lang.Object", "java.lang.String", "java.lang.CharSequence", "java.lang.Class",
            "java.lang.ClassLoader", "java.lang.Thread", "java.lang.Throwable",
            "java.lang.StackTraceElement", "java.nio.ByteBuffer",
            "java.util.Collection", "java.util.Iterator", "java.util.List", "java.util.Map",
            "java.util.Map$Entry", "java.util.Set",
            "java.lang.ArrayIndexOutOfBoundsException", "java.lang.ArrayStoreException",
            "java.lang.ClassCircularityError", "java.lang.ClassFormatError",
            "java.lang.ClassNotFoundException", "java.lang.ExceptionInInitializerError",
            "java.lang.IllegalArgumentException", "java.lang.IllegalMonitorStateException",
            "java.lang.InstantiationException", "java.lang.LinkageError",
            "java.lang.NoClassDefFoundError", "java.lang.NoSuchFieldError", "java.lang.NoSuchMethodError",
            "java.lang.NumberFormatException", "java.lang.OutOfMemoryError", "java.lang.RuntimeException",
            "java.lang.SecurityException", "java.lang.StringIndexOutOfBoundsException");

    /** Array classes the {@code jni} crate's array bindings look up. */
    private static final Set<String> JNI_CRATE_ARRAY_CLASSES = Set.of(
            "java.lang.Object[]", "java.lang.String[]", "java.lang.Throwable[]",
            "java.lang.StackTraceElement[]", "boolean[]", "byte[]");

    /**
     * Every JNI API on {@code jni::Env} that resolves a class, method or field
     * by name — each call site must be explained by an extraction rule below.
     */
    private static final Pattern LOOKUP_CALL = Pattern.compile(
            "\\.(find_class|load_class|lookup_class|get_method_id|get_static_method_id|get_field_id"
                    + "|get_static_field_id|call_method|call_static_method|call_nonvirtual_method"
                    + "|new_object|get_field|set_field|get_static_field|set_static_field|throw_new"
                    + "|throw|register_native_methods|is_instance_of)\\s*\\(");

    /** {@code CachedMethod::resolve(env, jni_str!(class), jni_str!(name), jni_sig!(sig))}. */
    private static final Pattern RESOLVE = Pattern.compile(
            "CachedMethod::resolve\\(\\s*\\w+\\s*,\\s*jni_str!\\(\\s*\"([^\"]+)\"\\s*\\)\\s*,"
                    + "\\s*jni_str!\\(\\s*\"([^\"]+)\"\\s*\\)\\s*,\\s*jni_sig!\\(\\s*\"([^\"]+)\"\\s*\\)");

    /** {@code env.call_method(obj, jni_str!(name), jni_sig!(sig), ..)} (checked fallback). */
    private static final Pattern CALL_METHOD = Pattern.compile(
            "\\.call_method\\(\\s*[\\w.&()]+\\s*,\\s*jni_str!\\(\\s*\"([^\"]+)\"\\s*\\)\\s*,"
                    + "\\s*jni_sig!\\(\\s*\"([^\"]+)\"\\s*\\)");

    /** {@code env.throw_new(jni_str!(class), ..)}. */
    private static final Pattern THROW_NEW = Pattern.compile(
            "\\.throw_new\\(\\s*(?:jni::)?jni_str!\\(\\s*\"([^\"]+)\"\\s*\\)");

    /** Any JNI class-name literal ({@code jni_str!("java/io/InputStream")}). */
    private static final Pattern CLASS_LITERAL = Pattern.compile(
            "jni_str!\\(\\s*\"([A-Za-z_$][\\w$]*(?:/[A-Za-z_$][\\w$]*)+)\"\\s*\\)");

    // ── Rust sources ────────────────────────────────────────────────────

    /** Non-test sources of {@code crates/vespera_jni/src}, line comments stripped. */
    private static Map<Path, String> rustSources() throws IOException {
        assertTrue(Files.isDirectory(JNI_SOURCES), "missing " + JNI_SOURCES.toAbsolutePath());
        Map<Path, String> sources = new TreeMap<>();
        try (Stream<Path> files = Files.walk(JNI_SOURCES)) {
            for (Path file : (Iterable<Path>) files::iterator) {
                String name = file.getFileName().toString();
                if (!name.endsWith(".rs") || name.endsWith("_tests.rs") || name.equals("tests.rs")) {
                    continue;
                }
                sources.put(file, stripLineComments(Files.readString(file, StandardCharsets.UTF_8)));
            }
        }
        assertFalse(sources.isEmpty(), "no Rust sources under " + JNI_SOURCES);
        return sources;
    }

    /** Drops {@code //}, {@code ///} and {@code //!} comments (doc examples are not code). */
    static String stripLineComments(String source) {
        StringBuilder out = new StringBuilder(source.length());
        for (String line : source.split("\n", -1)) {
            int comment = commentStart(line);
            out.append(comment < 0 ? line : line.substring(0, comment)).append('\n');
        }
        return out.toString();
    }

    private static int commentStart(String line) {
        boolean inString = false;
        for (int i = 0; i < line.length(); i++) {
            char c = line.charAt(i);
            if (inString) {
                if (c == '\\') {
                    i++;
                } else if (c == '"') {
                    inString = false;
                }
            } else if (c == '"') {
                inString = true;
            } else if (c == '/' && i + 1 < line.length() && line.charAt(i + 1) == '/') {
                return i;
            }
        }
        return -1;
    }

    private static List<String[]> matches(Pattern pattern, Map<Path, String> sources) {
        List<String[]> found = new ArrayList<>();
        for (String source : sources.values()) {
            Matcher m = pattern.matcher(source);
            while (m.find()) {
                String[] groups = new String[m.groupCount()];
                for (int g = 0; g < groups.length; g++) {
                    groups[g] = m.group(g + 1);
                }
                found.add(groups);
            }
        }
        return found;
    }

    // ── metadata ────────────────────────────────────────────────────────

    private static JsonNode metadata(String file) throws IOException {
        try (InputStream in = NativeImageMetadataTest.class.getClassLoader()
                .getResourceAsStream(METADATA_DIR + file)) {
            assertNotNull(in, METADATA_DIR + file + " is not on the classpath");
            return new ObjectMapper().readTree(in);
        }
    }

    private static JsonNode jniEntry(JsonNode jniConfig, String className) {
        for (JsonNode entry : jniConfig) {
            if (className.equals(entry.path("name").asText())) {
                return entry;
            }
        }
        return null;
    }

    private static boolean hasMethod(JsonNode entry, String name, List<String> parameterTypes) {
        if (entry == null) {
            return false;
        }
        // Registering all public members covers the (public java.*) members.
        return entry.path(name.equals("<init>") ? "allPublicConstructors" : "allPublicMethods").asBoolean()
                || hasExplicitMethod(entry, name, parameterTypes);
    }

    private static boolean hasExplicitMethod(JsonNode entry, String name, List<String> parameterTypes) {
        if (entry == null) {
            return false;
        }
        for (JsonNode method : entry.path("methods")) {
            if (!name.equals(method.path("name").asText())) {
                continue;
            }
            List<String> declared = new ArrayList<>();
            method.path("parameterTypes").forEach(t -> declared.add(t.asText()));
            if (declared.equals(parameterTypes)) {
                return true;
            }
        }
        return false;
    }

    /** {@code java/io/InputStream} → {@code java.io.InputStream}. */
    static String binaryName(String internalName) {
        return internalName.replace('/', '.');
    }

    /** Parameter types of a JNI method descriptor, in native-image config spelling. */
    static List<String> parameterTypes(String descriptor) {
        assertEquals('(', descriptor.charAt(0), descriptor);
        List<String> types = new ArrayList<>();
        int i = 1;
        while (descriptor.charAt(i) != ')') {
            int dims = 0;
            while (descriptor.charAt(i) == '[') {
                dims++;
                i++;
            }
            String type;
            char c = descriptor.charAt(i);
            if (c == 'L') {
                int end = descriptor.indexOf(';', i);
                type = binaryName(descriptor.substring(i + 1, end));
                i = end + 1;
            } else {
                type = switch (c) {
                    case 'Z' -> "boolean";
                    case 'B' -> "byte";
                    case 'C' -> "char";
                    case 'S' -> "short";
                    case 'I' -> "int";
                    case 'J' -> "long";
                    case 'F' -> "float";
                    case 'D' -> "double";
                    default -> throw new IllegalArgumentException(descriptor);
                };
                i++;
            }
            types.add(type + "[]".repeat(dims));
        }
        return types;
    }

    // ── JNI ─────────────────────────────────────────────────────────────

    @Test
    void descriptorParsing() {
        assertEquals(List.of("byte[]", "int", "int"), parameterTypes("([BII)V"));
        assertEquals(List.of("java.lang.Object"), parameterTypes("(Ljava/lang/Object;)Z"));
        assertEquals(List.of(), parameterTypes("()V"));
        assertEquals(List.of("java.lang.String[][]", "long"), parameterTypes("([[Ljava/lang/String;J)V"));
        assertEquals("let x = \"a//b\"; \n", stripLineComments("let x = \"a//b\"; // c"));
    }

    @Test
    void everyCachedJniMethodIsRegistered() throws IOException {
        JsonNode jni = metadata("jni-config.json");
        List<String[]> resolved = matches(RESOLVE, rustSources());
        assertFalse(resolved.isEmpty(), "found no CachedMethod::resolve(..) lookups — extraction is stale");
        for (String[] lookup : resolved) {
            String type = binaryName(lookup[0]);
            assertTrue(hasMethod(jniEntry(jni, type), lookup[1], parameterTypes(lookup[2])),
                    "jni-config.json lacks " + type + "." + lookup[1] + lookup[2]);
        }
    }

    @Test
    void everyCheckedCallMethodFallbackIsRegistered() throws IOException {
        // The string-based `call_method` fallbacks resolve the method on the
        // receiver's runtime class; native image walks the superclass chain to
        // the registered `java.*` declaration, so (name, sig) must be
        // registered on some class.
        JsonNode jni = metadata("jni-config.json");
        for (String[] call : matches(CALL_METHOD, rustSources())) {
            List<String> params = parameterTypes(call[1]);
            boolean registered = false;
            for (JsonNode entry : jni) {
                registered |= hasExplicitMethod(entry, call[0], params);
            }
            assertTrue(registered, "jni-config.json registers no " + call[0] + call[1]);
        }
    }

    @Test
    void everyThrownExceptionHasItsMessageConstructor() throws IOException {
        JsonNode jni = metadata("jni-config.json");
        Map<Path, String> sources = rustSources();
        Set<String> thrown = new TreeSet<>();
        matches(THROW_NEW, sources).forEach(m -> thrown.add(binaryName(m[0])));
        assertFalse(thrown.isEmpty(), "found no throw_new(..) — extraction is stale");
        if (sources.values().stream().anyMatch(s -> s.contains("ThrowRuntimeExAndDefault"))) {
            // `jni::errors::ThrowRuntimeExAndDefault` throws RuntimeException.
            thrown.add("java.lang.RuntimeException");
        }
        for (String type : thrown) {
            assertTrue(hasMethod(jniEntry(jni, type), "<init>", List.of("java.lang.String")),
                    "jni-config.json lacks " + type + "(String) (ThrowNew)");
        }
    }

    @Test
    void everyJniClassLiteralIsRegistered() throws IOException {
        JsonNode jni = metadata("jni-config.json");
        for (String[] literal : matches(CLASS_LITERAL, rustSources())) {
            String type = binaryName(literal[0]);
            assertNotNull(jniEntry(jni, type), "jni-config.json does not register " + type);
        }
    }

    @Test
    void everyJniLookupCallSiteIsCoveredByAnExtractionRule() throws IOException {
        Map<Path, String> sources = rustSources();
        Map<String, Integer> calls = new TreeMap<>();
        for (String source : sources.values()) {
            Matcher m = LOOKUP_CALL.matcher(source);
            while (m.find()) {
                calls.merge(m.group(1), 1, Integer::sum);
            }
        }
        // The only generic lookups are inside CachedMethod::resolve (one
        // find_class + one get_method_id, fed by the RESOLVE extraction).
        Map<String, Integer> explained = new TreeMap<>();
        explained.put("find_class", 1);
        explained.put("get_method_id", 1);
        int throwNew = matches(THROW_NEW, sources).size();
        if (throwNew > 0) {
            explained.put("throw_new", throwNew);
        }
        int callMethod = matches(CALL_METHOD, sources).size();
        if (callMethod > 0) {
            explained.put("call_method", callMethod);
        }
        assertEquals(explained, calls,
                "a JNI lookup in crates/vespera_jni is not covered by NativeImageMetadataTest: register it in "
                        + METADATA_DIR + "jni-config.json and extend the extraction rules");
    }

    @Test
    void jniCrateInternalsAreRegisteredForTheAuditedVersion() throws IOException {
        String manifest = Files.readString(JNI_SOURCES.resolveSibling("Cargo.toml"), StandardCharsets.UTF_8);
        Matcher dep = Pattern.compile("(?m)^jni\\s*=\\s*(?:\\{[^}]*version\\s*=\\s*)?\"([^\"]+)\"").matcher(manifest);
        assertTrue(dep.find(), "no `jni` dependency in crates/vespera_jni/Cargo.toml");
        assertEquals(AUDITED_JNI_CRATE, dep.group(1),
                "the jni crate changed: re-audit its internal JNI lookups (see AUDITED_JNI_CRATE)");
        JsonNode jni = metadata("jni-config.json");
        for (String type : JNI_CRATE_CLASSES) {
            JsonNode entry = jniEntry(jni, type);
            assertNotNull(entry, "jni-config.json does not register " + type);
            assertTrue(entry.path("allPublicMethods").asBoolean()
                            && entry.path("allPublicConstructors").asBoolean(),
                    type + " must register allPublicMethods + allPublicConstructors (jni crate bindings)");
        }
        for (String array : JNI_CRATE_ARRAY_CLASSES) {
            assertNotNull(jniEntry(jni, array), "jni-config.json does not register " + array);
        }
    }

    // ── resources / reflection ──────────────────────────────────────────

    @Test
    void bundledNativeLibrariesAndVersionResourceAreIncluded() throws IOException {
        List<Pattern> includes = new ArrayList<>();
        metadata("resource-config.json").path("resources").path("includes")
                .forEach(p -> includes.add(Pattern.compile(p.path("pattern").asText())));
        assertFalse(includes.isEmpty());
        List<String> requested = new ArrayList<>();
        for (String platform : List.of("linux-x86_64", "linux-aarch64-musl", "macos-aarch64", "windows-x86_64")) {
            for (String file : List.of("libdemo.so", "libdemo.dylib", "demo.dll")) {
                VesperaNativeLoader.resolveBundledResource(List.of(platform), file, path -> {
                    requested.add(path);
                    return new ByteArrayInputStream(new byte[0]);
                });
            }
        }
        requested.add("com/devfive/vespera/bridge/vespera-bridge.properties");
        for (String path : requested) {
            assertTrue(includes.stream().anyMatch(p -> p.matcher(path).matches()),
                    "resource-config.json does not include " + path);
        }
        assertFalse(includes.stream().anyMatch(p -> p.matcher("native/linux-x86_64/x/y.so").matches()),
                "resource pattern should stay narrow");
    }

    @Test
    void virtualThreadProbeIsRegisteredForReflection() throws IOException {
        // VesperaDirectBufferPool resolves Thread#isVirtual() via a MethodHandle
        // (the bridge compiles against Java 17).
        boolean found = false;
        for (JsonNode entry : metadata("reflect-config.json")) {
            found |= "java.lang.Thread".equals(entry.path("name").asText())
                    && hasMethod(entry, "isVirtual", List.of());
        }
        assertTrue(found, "reflect-config.json lacks java.lang.Thread#isVirtual()");
    }

    // ── Spring AOT ──────────────────────────────────────────────────────

    @Test
    void autoConfigurationImportsTheRuntimeHints() {
        ImportRuntimeHints imported =
                VesperaBridgeAutoConfiguration.class.getAnnotation(ImportRuntimeHints.class);
        assertNotNull(imported);
        assertTrue(List.of(imported.value()).contains(VesperaBridgeRuntimeHints.class));
    }

    @Test
    void runtimeHintsCoverProxyMethodLookup() {
        RuntimeHints hints = new RuntimeHints();
        new VesperaBridgeRuntimeHints().registerHints(hints, getClass().getClassLoader());
        assertInvokable(hints, VesperaProxyController.class.getName(), "proxy");
    }

    @Test
    void runtimeHintsCoverSpringSecurityAuthoritiesOnlyWhenPresent() {
        RuntimeHints absent = new RuntimeHints();
        new VesperaBridgeRuntimeHints().registerHints(absent, getClass().getClassLoader());
        // Spring Security is not on this test classpath.
        assertEquals(null, absent.reflection().getTypeHint(TypeReference.of(ServletPrincipalResolver.AUTHENTICATION)));

        RuntimeHints present = new RuntimeHints();
        VesperaBridgeRuntimeHints.registerHints(present, type -> true);
        assertInvokable(present, ServletPrincipalResolver.AUTHENTICATION, "getAuthorities");
        assertInvokable(present, ServletPrincipalResolver.GRANTED_AUTHORITY, "getAuthority");
    }

    private static void assertInvokable(RuntimeHints hints, String type, String method) {
        TypeHint hint = hints.reflection().getTypeHint(TypeReference.of(type));
        assertNotNull(hint, "no reflection hint for " + type);
        boolean invokable = hint.methods().anyMatch(
                (ExecutableHint m) -> m.getName().equals(method) && m.getMode() == ExecutableMode.INVOKE);
        if (!invokable) {
            fail("no INVOKE hint for " + type + "#" + method);
        }
    }
}
