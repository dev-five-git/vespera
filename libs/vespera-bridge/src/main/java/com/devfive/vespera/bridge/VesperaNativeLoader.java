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

    @FunctionalInterface
    interface TempFileDeleter {
        void delete(Path path) throws IOException;
    }

    static void loadBundled(String libraryName) {
        String os = detectOs();
        String arch = detectArch();
        String filename = mapLibraryName(os, libraryName);
        String resourcePath = "native/" + os + "-" + arch + "/" + filename;

        MessageDigest digest = messageDigest("SHA-256");

        try (InputStream in =
                VesperaBridge.class.getClassLoader().getResourceAsStream(resourcePath)) {
            if (in == null) {
                throw new BundledNativeAbsent("Not found in JAR: " + resourcePath);
            }
            String suffix = filename.substring(filename.lastIndexOf('.'));
            Path temp = Files.createTempFile("vespera-", suffix);
            boolean loaded = false;

            try {
                long copiedBytes;
                try (DigestInputStream din = new DigestInputStream(in, digest)) {
                    copiedBytes = Files.copy(din, temp, StandardCopyOption.REPLACE_EXISTING);
                }
                byte[] resourceDigest = digest.digest();
                long extractedBytes = Files.size(temp);
                requireMatchingSize(resourcePath, copiedBytes, extractedBytes);
                if (Boolean.getBoolean("vespera.native.verifyExtractedDigest")) {
                    byte[] extractedDigest = digestOfFile(temp, digest);
                    requireMatchingDigest(resourcePath, resourceDigest, extractedDigest);
                }

                System.load(temp.toAbsolutePath().toString());
                loaded = true;
                temp.toFile().deleteOnExit();
            } finally {
                if (!loaded) {
                    deleteAfterFailedLoad(temp, Files::deleteIfExists);
                }
            }
        } catch (IOException e) {
            UnsatisfiedLinkError ule = new UnsatisfiedLinkError("Extract failed: " + e.getMessage());
            ule.initCause(e);
            throw ule;
        }
    }

    static MessageDigest messageDigest(String algorithm) {
        try {
            return MessageDigest.getInstance(algorithm);
        } catch (NoSuchAlgorithmException digestMissing) {
            throw new UnsatisfiedLinkError(
                    algorithm + " unavailable for native library verification: "
                            + digestMissing.getMessage());
        }
    }

    static void requireMatchingSize(String resourcePath, long copiedBytes, long extractedBytes) {
        if (copiedBytes != extractedBytes) {
            throw new UnsatisfiedLinkError("Native library extraction failed for " + resourcePath
                    + ": copied " + copiedBytes + " bytes but extracted file has "
                    + extractedBytes + " bytes.");
        }
    }

    static void requireMatchingDigest(
            String resourcePath, byte[] resourceDigest, byte[] extractedDigest) {
        if (!MessageDigest.isEqual(resourceDigest, extractedDigest)) {
            throw new UnsatisfiedLinkError(
                    "Native library integrity check failed for " + resourcePath
                            + ": extracted file does not match the bundled resource "
                            + "(corrupted or modified extraction).");
        }
    }

    static void deleteAfterFailedLoad(Path temp, TempFileDeleter deleter) {
        try {
            deleter.delete(temp);
        } catch (IOException deleteFailure) {
            // The load failure is more important; leaving the temp file behind
            // must not mask the root cause the caller is about to report.
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
