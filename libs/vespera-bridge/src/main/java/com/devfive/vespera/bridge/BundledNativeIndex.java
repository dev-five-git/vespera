package com.devfive.vespera.bridge;

import java.io.IOException;
import java.net.JarURLConnection;
import java.net.URISyntaxException;
import java.net.URL;
import java.net.URLConnection;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.List;
import java.util.Map;
import java.util.SortedMap;
import java.util.SortedSet;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.jar.JarEntry;
import java.util.jar.JarFile;

/**
 * Diagnostics for a missing bundled native library: which
 * {@code native/<platform>/<file>} resources the classpath DOES carry (the
 * app jar, or per-platform native jars such as
 * {@code myapp-native-0.1.0-linux-x86_64.jar}), so the error names the
 * platforms found next to the host platform instead of only the paths tried.
 *
 * <p>Only used on the failure path; every probe is best effort and never
 * throws.
 */
final class BundledNativeIndex {

    /**
     * Platforms probed by exact resource name, for jars built without
     * directory entries (which {@link ClassLoader#getResources} cannot list).
     */
    static final List<String> KNOWN_PLATFORMS = List.of(
            "linux-x86_64", "linux-aarch64", "linux-x86_64-musl", "linux-aarch64-musl",
            "linux-arm", "linux-riscv64", "linux-ppc64le", "linux-s390x",
            "macos-x86_64", "macos-aarch64",
            "windows-x86_64", "windows-aarch64", "windows-x86");

    private BundledNativeIndex() {}

    /** {@code platform -> library file names} of every {@code native/<platform>/<file>} found. */
    static SortedMap<String, SortedSet<String>> scan(ClassLoader loader, String filename) {
        SortedMap<String, SortedSet<String>> found = new TreeMap<>();
        if (loader == null) {
            return found;
        }
        try {
            Enumeration<URL> roots = loader.getResources("native/");
            while (roots.hasMoreElements()) {
                list(roots.nextElement(), found);
            }
        } catch (IOException | RuntimeException unlistable) {
            // Best effort: fall back to the known-platform probes below.
        }
        for (String platform : KNOWN_PLATFORMS) {
            try {
                if (loader.getResource("native/" + platform + "/" + filename) != null) {
                    add(found, platform, filename);
                }
            } catch (RuntimeException ignored) {
                // Best effort.
            }
        }
        return found;
    }

    private static void list(URL root, Map<String, SortedSet<String>> found) {
        try {
            if ("file".equals(root.getProtocol())) {
                listDirectory(Paths.get(root.toURI()), found);
                return;
            }
            URLConnection connection = root.openConnection();
            if (connection instanceof JarURLConnection jarConnection) {
                jarConnection.setUseCaches(false);
                try (JarFile jar = jarConnection.getJarFile()) {
                    Enumeration<JarEntry> entries = jar.entries();
                    while (entries.hasMoreElements()) {
                        addEntry(entries.nextElement().getName(), found);
                    }
                }
            }
        } catch (IOException | URISyntaxException | RuntimeException unlistable) {
            // Best effort: an unreadable root is skipped.
        }
    }

    private static void listDirectory(Path nativeDir, Map<String, SortedSet<String>> found)
            throws IOException {
        try (DirectoryStream<Path> platforms = Files.newDirectoryStream(nativeDir, Files::isDirectory)) {
            for (Path platform : platforms) {
                try (DirectoryStream<Path> files = Files.newDirectoryStream(platform, Files::isRegularFile)) {
                    for (Path file : files) {
                        add(found, platform.getFileName().toString(), file.getFileName().toString());
                    }
                }
            }
        }
    }

    /** Record a jar entry named exactly {@code native/<platform>/<file>}. */
    static void addEntry(String name, Map<String, SortedSet<String>> found) {
        if (!name.startsWith("native/")) {
            return;
        }
        String[] parts = name.substring("native/".length()).split("/", -1);
        if (parts.length == 2 && !parts[0].isEmpty() && !parts[1].isEmpty()) {
            add(found, parts[0], parts[1]);
        }
    }

    private static void add(Map<String, SortedSet<String>> found, String platform, String file) {
        found.computeIfAbsent(platform, p -> new TreeSet<>()).add(file);
    }

    /**
     * The explanation appended to "Not found in JAR: ...": the platforms that
     * bundle {@code filename}, or every native library found when none does.
     */
    static String describe(String hostPlatform, String filename, Map<String, SortedSet<String>> found) {
        List<String> withLibrary = new ArrayList<>();
        List<String> others = new ArrayList<>();
        found.forEach((platform, files) -> {
            if (files.contains(filename)) {
                withLibrary.add(platform);
            }
            for (String file : files) {
                if (!file.equals(filename)) {
                    others.add("native/" + platform + "/" + file);
                }
            }
        });
        String fix = " Add the native jar for " + hostPlatform + " to the runtime classpath"
                + " (e.g. runtimeOnly(\"<group>:<artifact>:<version>:" + hostPlatform + "\"),"
                + " published by the Gradle plugin's vespera.nativeJarPlatforms) or bundle that"
                + " build into the application jar.";
        if (!withLibrary.isEmpty()) {
            return ". Host platform is " + hostPlatform + "; the classpath bundles " + filename
                    + " only for " + withLibrary + "." + fix;
        }
        if (!others.isEmpty()) {
            return ". Host platform is " + hostPlatform + "; no platform bundles " + filename
                    + ", but the classpath has " + others
                    + " (does the VesperaBridge.init library name match the cdylib?).";
        }
        return ". Host platform is " + hostPlatform + "; the classpath bundles no native/<platform>/"
                + " library at all." + fix;
    }
}
