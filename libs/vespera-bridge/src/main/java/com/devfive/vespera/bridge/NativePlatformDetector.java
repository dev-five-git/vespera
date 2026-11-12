package com.devfive.vespera.bridge;

import java.io.BufferedReader;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * Resolves the {@code native/<platform>/} resource folders the bundled cdylib
 * is looked up in: {@code <os>-<arch>}, plus a {@code -musl} classifier on a
 * musl-libc Linux JVM (Alpine). A glibc {@code .so} cannot be loaded into a
 * musl process (and vice versa), so the two need separate folders.
 *
 * <p><strong>libc detection.</strong> In order:
 * <ol>
 *   <li>System property {@value #LIBC_PROPERTY} ({@code musl} or
 *       {@code glibc}/{@code gnu}) — an explicit override for sandboxes
 *       without {@code /proc}.
 *   <li>{@code /proc/self/maps} — the libraries mapped into <em>this</em>
 *       JVM process. It is the only no-process probe that answers the actual
 *       question ("which libc is this JVM linked against?"): a musl process
 *       maps its loader/libc {@code ld-musl-<arch>.so.1}, a glibc process
 *       {@code libc.so.6} / {@code ld-linux*}. File-existence probes are only
 *       indirect: Debian/Ubuntu's {@code musl} package installs
 *       {@code /lib/ld-musl-*.so.1} on glibc systems, and Alpine's
 *       {@code gcompat} / a glibc-based JDK on Alpine has the opposite
 *       problem. Spawning {@code ldd --version} costs a fork+exec and depends
 *       on {@code ldd} being installed (it is not in minimal images).
 *   <li>Fallback when {@code /proc} is unreadable: musl iff a
 *       {@code ld-musl-*} loader exists in {@code /lib} and no glibc
 *       {@code ld-linux*} loader exists in {@code /lib} / {@code /lib64}.
 * </ol>
 * Detection runs once per {@code loadBundled} call (once per JVM in practice)
 * and only on Linux.
 */
final class NativePlatformDetector {

    /** System property forcing the libc: {@code musl} or {@code glibc} / {@code gnu}. */
    static final String LIBC_PROPERTY = "vespera.native.libc";
    /** Folder classifier appended to {@code linux-<arch>} on a musl JVM. */
    static final String MUSL_SUFFIX = "-musl";

    /** Host file system access, injectable so tests do not depend on the host. */
    interface LibcProbe {
        /** Lines of {@code /proc/self/maps}, or {@code null} when unreadable. */
        List<String> selfMaps();

        /** File names in {@code dir}; empty when it is missing or unreadable. */
        List<String> listDir(String dir);
    }

    /** The real host probe. */
    static final LibcProbe HOST = new LibcProbe() {
        @Override
        public List<String> selfMaps() {
            try (BufferedReader reader = Files.newBufferedReader(
                    Paths.get("/proc/self/maps"), StandardCharsets.ISO_8859_1)) {
                List<String> lines = new ArrayList<>();
                for (String line; (line = reader.readLine()) != null; ) {
                    lines.add(line);
                }
                return lines;
            } catch (IOException | SecurityException | UnsupportedOperationException unreadable) {
                return null;
            }
        }

        @Override
        public List<String> listDir(String dir) {
            List<String> names = new ArrayList<>();
            try (DirectoryStream<Path> entries = Files.newDirectoryStream(Paths.get(dir))) {
                for (Path entry : entries) {
                    names.add(entry.getFileName().toString());
                }
            } catch (IOException | SecurityException | UnsupportedOperationException unreadable) {
                // Missing / unreadable directory: no names.
            }
            return names;
        }
    };

    private NativePlatformDetector() {}

    static String detectOs(String osName) {
        String os = osName.toLowerCase(Locale.ROOT);
        // "darwin" MUST be tested before "win": the string "darwin" contains
        // the substring "win", so the opposite order sends a Darwin-reporting
        // JVM down the Windows path and makes it look for a .dll.
        if (os.contains("mac") || os.contains("darwin")) return "macos";
        if (os.contains("win")) return "windows";
        return "linux";
    }

    static String detectArch(String osArch) {
        String arch = osArch.toLowerCase(Locale.ROOT);
        if (arch.contains("amd64") || arch.contains("x86_64")) return "x86_64";
        if (arch.contains("aarch64") || arch.contains("arm64")) return "aarch64";
        return arch;
    }

    /**
     * Whether this Linux JVM runs on musl libc ({@code override} is the
     * {@value #LIBC_PROPERTY} value, {@code null} when unset).
     */
    static boolean isMusl(String override, LibcProbe probe) {
        if (override != null && !override.isBlank()) {
            String libc = override.trim().toLowerCase(Locale.ROOT);
            if (libc.equals("musl")) return true;
            if (libc.equals("glibc") || libc.equals("gnu")) return false;
            // Unknown value: ignore it and detect.
        }
        List<String> maps = probe.selfMaps();
        if (maps != null) {
            for (String line : maps) {
                String file = mappedFileName(line);
                if (file == null) continue;
                if (file.startsWith("ld-musl-") || file.startsWith("libc.musl-")) return true;
                if (file.startsWith("libc.so.6") || file.startsWith("libc-2.")
                        || file.startsWith("ld-linux")) {
                    return false;
                }
            }
            // Readable maps without a recognisable libc (a static launcher?):
            // fall through to the file-system heuristic.
        }
        boolean muslLoader = probe.listDir("/lib").stream().anyMatch(n -> n.startsWith("ld-musl-"));
        if (!muslLoader) return false;
        boolean glibcLoader = probe.listDir("/lib").stream().anyMatch(n -> n.startsWith("ld-linux"))
                || probe.listDir("/lib64").stream().anyMatch(n -> n.startsWith("ld-linux"));
        return !glibcLoader;
    }

    /** File name of the path column of a {@code /proc/self/maps} line, or {@code null}. */
    static String mappedFileName(String line) {
        int slash = line.indexOf('/');
        if (slash < 0) return null;
        String path = line.substring(slash).trim();
        if (path.endsWith(" (deleted)")) {
            path = path.substring(0, path.length() - " (deleted)".length());
        }
        int last = path.lastIndexOf('/');
        return last < 0 || last == path.length() - 1 ? null : path.substring(last + 1);
    }

    /**
     * Resource folders to try, most specific first: {@code [linux-<arch>-musl,
     * linux-<arch>]} on a musl JVM, otherwise the single {@code <os>-<arch>}.
     */
    static List<String> candidatePlatforms(String os, String arch, boolean musl) {
        String base = os + "-" + arch;
        return "linux".equals(os) && musl ? List.of(base + MUSL_SUFFIX, base) : List.of(base);
    }

    /** {@link #candidatePlatforms} for the running JVM. */
    static List<String> hostCandidatePlatforms(LibcProbe probe) {
        String os = detectOs(System.getProperty("os.name", ""));
        String arch = detectArch(System.getProperty("os.arch", ""));
        boolean musl = "linux".equals(os) && isMusl(System.getProperty(LIBC_PROPERTY), probe);
        return candidatePlatforms(os, arch, musl);
    }
}
