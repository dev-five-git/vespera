package com.devfive.vespera.bridge;

import java.io.*;
import java.nio.file.*;

/**
 * JNI bridge to any Rust cdylib built with vespera's JNI feature.
 *
 * <h3>Usage — single line in your Spring Boot app</h3>
 * <pre>{@code
 * VesperaBridge.init("rust_jni_demo");
 * }</pre>
 *
 * <p>That's it. The proxy controller ({@link VesperaProxyController})
 * is auto-configured by Spring's component scan when this JAR is on
 * the classpath.
 */
public class VesperaBridge {

    private static volatile boolean loaded = false;

    /**
     * Initialize the Rust engine.  Tries bundled (JAR-embedded) first,
     * falls back to {@code java.library.path}.
     *
     * @param libraryName Cargo crate name (e.g. "rust_jni_demo")
     */
    public static synchronized void init(String libraryName) {
        if (loaded) return;
        try {
            loadBundled(libraryName);
        } catch (UnsatisfiedLinkError e) {
            System.loadLibrary(libraryName);
        }
        loaded = true;
    }

    /**
     * Dispatch a full HTTP-like request through the Rust axum router.
     */
    public static native String dispatch(String requestEnvelopeJson);

    // ── Internal: bundled native lib extraction ──────────────────────

    private static void loadBundled(String libraryName) {
        String os = detectOs();
        String arch = detectArch();
        String filename = mapLibraryName(os, libraryName);
        String resourcePath = "native/" + os + "-" + arch + "/" + filename;

        try (InputStream in = VesperaBridge.class.getClassLoader().getResourceAsStream(resourcePath)) {
            if (in == null) {
                throw new UnsatisfiedLinkError("Not found in JAR: " + resourcePath);
            }
            String suffix = filename.substring(filename.lastIndexOf('.'));
            Path temp = Files.createTempFile("vespera-", suffix);
            temp.toFile().deleteOnExit();
            Files.copy(in, temp, StandardCopyOption.REPLACE_EXISTING);
            System.load(temp.toAbsolutePath().toString());
        } catch (IOException e) {
            throw new UnsatisfiedLinkError("Extract failed: " + e.getMessage());
        }
    }

    private static String detectOs() {
        String os = System.getProperty("os.name", "").toLowerCase();
        if (os.contains("win")) return "windows";
        if (os.contains("mac") || os.contains("darwin")) return "macos";
        return "linux";
    }

    private static String detectArch() {
        String arch = System.getProperty("os.arch", "").toLowerCase();
        if (arch.contains("amd64") || arch.contains("x86_64")) return "x86_64";
        if (arch.contains("aarch64") || arch.contains("arm64")) return "aarch64";
        return arch;
    }

    private static String mapLibraryName(String os, String name) {
        return switch (os) {
            case "windows" -> name + ".dll";
            case "macos" -> "lib" + name + ".dylib";
            default -> "lib" + name + ".so";
        };
    }

    private VesperaBridge() {}
}
