package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Enumeration;
import java.util.List;
import java.util.Map;
import java.util.SortedMap;
import java.util.SortedSet;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.jar.JarEntry;
import java.util.jar.JarOutputStream;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

/** Per-platform native jars on the classpath: lookup + "which platforms are there" diagnostics. */
class BundledNativeIndexTest {

    @TempDir
    Path tmp;

    /** A classifier jar like {@code myapp-native-0.1.0-<platform>.jar}. */
    private Path nativeJar(String platform, String file, boolean directoryEntries) throws IOException {
        Path jar = tmp.resolve("myapp-native-0.1.0-" + platform + ".jar");
        try (OutputStream out = Files.newOutputStream(jar); JarOutputStream zip = new JarOutputStream(out)) {
            if (directoryEntries) {
                zip.putNextEntry(new JarEntry("native/"));
                zip.closeEntry();
                zip.putNextEntry(new JarEntry("native/" + platform + "/"));
                zip.closeEntry();
            }
            zip.putNextEntry(new JarEntry("native/" + platform + "/" + file));
            zip.write(("lib for " + platform).getBytes(StandardCharsets.UTF_8));
            zip.closeEntry();
            zip.putNextEntry(new JarEntry("META-INF/vespera.txt"));
            zip.closeEntry();
        }
        return jar;
    }

    private URLClassLoader classpath(Path... roots) throws IOException {
        URL[] urls = new URL[roots.length];
        for (int i = 0; i < roots.length; i++) {
            urls[i] = roots[i].toUri().toURL();
        }
        return new URLClassLoader(urls, null);
    }

    @Test
    void scanListsEveryClassifierJarDirectoryAndUnlistableJar() throws IOException {
        Path dir = tmp.resolve("classes");
        Files.createDirectories(dir.resolve("native/linux-aarch64"));
        Files.writeString(dir.resolve("native/linux-aarch64/libdemo.so"), "dir");
        Files.writeString(dir.resolve("native/README.txt"), "not a platform folder");
        try (URLClassLoader loader = classpath(
                nativeJar("linux-x86_64", "libdemo.so", true),
                nativeJar("windows-x86_64", "other.dll", true),
                // No directory entries: invisible to getResources("native/"),
                // found by the known-platform probe instead.
                nativeJar("macos-aarch64", "libdemo.dylib", false),
                dir)) {
            SortedMap<String, SortedSet<String>> found = BundledNativeIndex.scan(loader, "libdemo.dylib");

            assertEquals(new TreeMap<>(Map.of(
                    "linux-aarch64", new TreeSet<>(List.of("libdemo.so")),
                    "linux-x86_64", new TreeSet<>(List.of("libdemo.so")),
                    "macos-aarch64", new TreeSet<>(List.of("libdemo.dylib")),
                    "windows-x86_64", new TreeSet<>(List.of("other.dll")))), found);
        }
        assertTrue(BundledNativeIndex.scan(null, "x").isEmpty());
    }

    @Test
    void scanSurvivesAFailingClassLoader() {
        ClassLoader broken = new ClassLoader(null) {
            @Override
            public Enumeration<URL> getResources(String name) throws IOException {
                throw new IOException("boom");
            }

            @Override
            public URL getResource(String name) {
                throw new IllegalStateException("boom");
            }
        };
        assertTrue(BundledNativeIndex.scan(broken, "libdemo.so").isEmpty());
    }

    @Test
    void classifierJarOnTheClasspathResolvesForItsPlatformOnly() throws IOException {
        try (URLClassLoader loader = classpath(
                nativeJar("linux-x86_64", "libdemo.so", true),
                nativeJar("linux-aarch64-musl", "libdemo.so", true))) {
            VesperaNativeLoader.BundledResource musl = VesperaNativeLoader.resolveBundledResource(
                    List.of("linux-aarch64-musl", "linux-aarch64"), "libdemo.so", loader::getResourceAsStream);
            try (InputStream in = musl.stream()) {
                assertEquals("native/linux-aarch64-musl/libdemo.so", musl.path());
                assertEquals("lib for linux-aarch64-musl", new String(in.readAllBytes(), StandardCharsets.UTF_8));
            }
            assertThrows(VesperaNativeLoader.BundledNativeAbsent.class,
                    () -> VesperaNativeLoader.resolveBundledResource(
                            List.of("macos-aarch64"), "libdemo.dylib", loader::getResourceAsStream));
        }
    }

    @Test
    void addEntryKeepsOnlyNativePlatformFiles() {
        SortedMap<String, SortedSet<String>> found = new TreeMap<>();
        for (String name : List.of("native/linux-x86_64/libdemo.so", "native/", "native/linux-x86_64/",
                "native/x.so", "native/a/b/c.so", "other/linux-x86_64/libdemo.so", "native//x.so")) {
            BundledNativeIndex.addEntry(name, found);
        }
        assertEquals(new TreeMap<>(Map.of("linux-x86_64", new TreeSet<>(List.of("libdemo.so")))), found);
    }

    @Test
    void describeNamesThePlatformsFoundNextToTheHost() {
        Map<String, SortedSet<String>> found = new TreeMap<>(Map.of(
                "macos-aarch64", new TreeSet<>(List.of("libdemo.so")),
                "windows-x86_64", new TreeSet<>(List.of("libdemo.so"))));

        String other = BundledNativeIndex.describe("linux-x86_64", "libdemo.so", found);
        assertTrue(other.contains("Host platform is linux-x86_64"), other);
        assertTrue(other.contains("libdemo.so only for [macos-aarch64, windows-x86_64]"), other);
        assertTrue(other.contains("runtimeOnly(\"<group>:<artifact>:<version>:linux-x86_64\")"), other);

        String misnamed = BundledNativeIndex.describe("linux-x86_64", "libwrong.so", found);
        assertTrue(misnamed.contains("no platform bundles libwrong.so"), misnamed);
        assertTrue(misnamed.contains("native/macos-aarch64/libdemo.so"), misnamed);

        String none = BundledNativeIndex.describe("linux-x86_64", "libdemo.so", Map.of());
        assertTrue(none.contains("no native/<platform>/ library at all"), none);
    }

    @Test
    void absentBundledLibraryMessageCarriesTheDiagnostics() {
        VesperaNativeLoader.BundledNativeAbsent error = assertThrows(
                VesperaNativeLoader.BundledNativeAbsent.class,
                () -> VesperaNativeLoader.loadBundled("absent_lib"));
        assertTrue(error.getMessage().startsWith("Not found in JAR: native/"), error.getMessage());
        assertTrue(error.getMessage().contains(". Host platform is "), error.getMessage());
    }

    @Test
    void systemFallbackFailureKeepsBothCauses() {
        VesperaNativeLoader.BundledNativeAbsent absent =
                new VesperaNativeLoader.BundledNativeAbsent("Not found in JAR: native/x/libdemo.so. Host platform is x.");
        UnsatisfiedLinkError notOnPath = new UnsatisfiedLinkError("no demo in java.library.path");

        UnsatisfiedLinkError combined = assertThrows(UnsatisfiedLinkError.class,
                () -> VesperaBridge.loadNativeLibrary("demo",
                        name -> { throw absent; }, name -> { throw notOnPath; }));

        assertTrue(combined.getMessage().startsWith("Cannot load native library 'demo': Not found in JAR"),
                combined.getMessage());
        assertTrue(combined.getMessage().endsWith("System library path fallback: no demo in java.library.path"),
                combined.getMessage());
        assertSame(notOnPath, combined.getCause());
        assertInstanceOf(VesperaNativeLoader.BundledNativeAbsent.class, combined.getSuppressed()[0]);

        // A successful fallback is silent.
        VesperaNativeLoader.loadFromSystemPath("demo", absent, name -> { });
    }
}
