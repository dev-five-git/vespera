package com.devfive.vespera.bridge;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.DigestInputStream;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;

/** Native library lookup/extraction helpers for {@link VesperaBridge}. */
final class VesperaNativeLoader {

    private VesperaNativeLoader() {}

    /**
     * Signals the bundled native library is genuinely ABSENT from the
     * classpath — the one legitimate reason to fall back to the system
     * library path.
     */
    static final class BundledNativeAbsent extends RuntimeException {
        BundledNativeAbsent(String message) {
            super(message);
        }
    }

    static void loadBundled(String libraryName) {
        String os = detectOs();
        String arch = detectArch();
        String filename = mapLibraryName(os, libraryName);
        String resourcePath = "native/" + os + "-" + arch + "/" + filename;

        MessageDigest digest;
        try {
            digest = MessageDigest.getInstance("SHA-256");
        } catch (NoSuchAlgorithmException sha256Missing) {
            throw new UnsatisfiedLinkError(
                    "SHA-256 unavailable for native library verification: "
                            + sha256Missing.getMessage());
        }

        try (InputStream in =
                VesperaBridge.class.getClassLoader().getResourceAsStream(resourcePath)) {
            if (in == null) {
                throw new BundledNativeAbsent("Not found in JAR: " + resourcePath);
            }
            String suffix = filename.substring(filename.lastIndexOf('.'));
            Path temp = Files.createTempFile("vespera-", suffix);
            boolean loaded = false;

            try {
                try (DigestInputStream din = new DigestInputStream(in, digest)) {
                    Files.copy(din, temp, StandardCopyOption.REPLACE_EXISTING);
                }
                byte[] resourceDigest = digest.digest();
                byte[] extractedDigest = digestOfFile(temp, digest);
                if (!MessageDigest.isEqual(resourceDigest, extractedDigest)) {
                    throw new UnsatisfiedLinkError(
                            "Native library integrity check failed for " + resourcePath
                                    + ": extracted file does not match the bundled resource "
                                    + "(corrupted or modified extraction).");
                }

                System.load(temp.toAbsolutePath().toString());
                loaded = true;
                temp.toFile().deleteOnExit();
            } finally {
                if (!loaded) {
                    try {
                        Files.deleteIfExists(temp);
                    } catch (IOException deleteFailure) {
                        // The load failure is more important; the temp path is
                        // still deleteOnExit-free, so do not mask the root cause.
                    }
                }
            }
        } catch (IOException e) {
            UnsatisfiedLinkError ule = new UnsatisfiedLinkError("Extract failed: " + e.getMessage());
            ule.initCause(e);
            throw ule;
        }
    }

    private static byte[] digestOfFile(Path file, MessageDigest digest) throws IOException {
        digest.reset();
        try (InputStream fin = Files.newInputStream(file)) {
            byte[] buf = new byte[64 * 1024];
            int n;
            while ((n = fin.read(buf)) != -1) {
                digest.update(buf, 0, n);
            }
        }
        return digest.digest();
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
}
