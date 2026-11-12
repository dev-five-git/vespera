package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.CsvSource;

class NativePlatformDetectorTest {

    /** Fake host: fixed maps lines (null = unreadable) and directory listings. */
    private static NativePlatformDetector.LibcProbe probe(
            List<String> maps, Map<String, List<String>> dirs) {
        return new NativePlatformDetector.LibcProbe() {
            @Override
            public List<String> selfMaps() {
                return maps;
            }

            @Override
            public List<String> listDir(String dir) {
                return dirs.getOrDefault(dir, List.of());
            }
        };
    }

    private static final List<String> MUSL_MAPS = List.of(
            "55d0c0a00000-55d0c0a01000 r--p 00000000 00:2f 1234  /usr/lib/jvm/java-17-openjdk/bin/java",
            "7f1c2a000000-7f1c2a100000 rw-p 00000000 00:00 0 ",
            "7f1c2b000000-7f1c2b014000 r--p 00000000 00:2f 5678  /lib/ld-musl-x86_64.so.1",
            "7ffd1c000000-7ffd1c021000 rw-p 00000000 00:00 0  [stack]");

    private static final List<String> GLIBC_MAPS = List.of(
            "55d0c0a00000-55d0c0a01000 r--p 00000000 08:01 1234  /usr/lib/jvm/java-17/bin/java",
            "7f1c2b000000-7f1c2b028000 r--p 00000000 08:01 5678  /usr/lib/x86_64-linux-gnu/libc.so.6",
            "7f1c2c000000-7f1c2c002000 r--p 00000000 08:01 9999  /usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2");

    @Test
    void procMapsWithMuslLoaderIsMusl() {
        assertTrue(NativePlatformDetector.isMusl(null, probe(MUSL_MAPS, Map.of())));
    }

    @Test
    void procMapsWithGlibcIsGlibcEvenWhenAMuslLoaderIsInstalled() {
        // Debian's `musl` package installs /lib/ld-musl-*.so.1 on a glibc
        // system: the mapped libc of this process must win.
        var dirs = Map.of("/lib", List.of("ld-musl-x86_64.so.1"));
        assertFalse(NativePlatformDetector.isMusl(null, probe(GLIBC_MAPS, dirs)));
    }

    @Test
    void oldGlibcVersionedLibcIsRecognised() {
        var maps = List.of("7f00-7f01 r-xp 00000000 08:01 1 /lib/x86_64-linux-gnu/libc-2.31.so");
        var dirs = Map.of("/lib", List.of("ld-musl-x86_64.so.1"));
        assertFalse(NativePlatformDetector.isMusl(null, probe(maps, dirs)));
    }

    @Test
    void muslLibcAliasInMapsIsMusl() {
        var maps = List.of("7f00-7f01 r-xp 00000000 08:01 1 /lib/libc.musl-aarch64.so.1 (deleted)");
        assertTrue(NativePlatformDetector.isMusl(null, probe(maps, Map.of())));
    }

    @Test
    void unreadableProcFallsBackToTheLoaderFiles() {
        var alpine = Map.of("/lib", List.of("apk", "ld-musl-aarch64.so.1", "libc.musl-aarch64.so.1"));
        assertTrue(NativePlatformDetector.isMusl(null, probe(null, alpine)));

        var debianWithMusl = Map.of(
                "/lib", List.of("ld-musl-x86_64.so.1"),
                "/lib64", List.of("ld-linux-x86-64.so.2"));
        assertFalse(NativePlatformDetector.isMusl(null, probe(null, debianWithMusl)));

        assertFalse(NativePlatformDetector.isMusl(null, probe(null, Map.of())));
    }

    @Test
    void mapsWithoutALibcFallBackToTheLoaderFiles() {
        var maps = List.of("7f00-7f01 r-xp 00000000 08:01 1 /opt/app/static-java", "7f02-7f03 rw-p 0 0 0 [heap]");
        var alpine = Map.of("/lib", List.of("ld-musl-x86_64.so.1"));
        assertTrue(NativePlatformDetector.isMusl(null, probe(maps, alpine)));
        assertFalse(NativePlatformDetector.isMusl(null, probe(maps, Map.of())));
    }

    @ParameterizedTest
    @CsvSource({"musl, true", " MUSL , true", "glibc, false", "gnu, false"})
    void explicitOverrideWinsWithoutProbing(String override, boolean expected) {
        var exploding = new NativePlatformDetector.LibcProbe() {
            @Override
            public List<String> selfMaps() {
                throw new AssertionError("must not probe");
            }

            @Override
            public List<String> listDir(String dir) {
                throw new AssertionError("must not probe");
            }
        };
        assertEquals(expected, NativePlatformDetector.isMusl(override, exploding));
    }

    @Test
    void unknownOrBlankOverrideIsIgnored() {
        assertTrue(NativePlatformDetector.isMusl("uclibc", probe(MUSL_MAPS, Map.of())));
        assertFalse(NativePlatformDetector.isMusl(" ", probe(GLIBC_MAPS, Map.of())));
    }

    @Test
    void mappedFileNameParsesTheMapsPathColumn() {
        assertEquals("ld-musl-x86_64.so.1", NativePlatformDetector.mappedFileName(MUSL_MAPS.get(2)));
        assertEquals("libc.so.6",
                NativePlatformDetector.mappedFileName("7f-80 r--p 0 08:01 5 /lib/libc.so.6 (deleted)"));
        assertNull(NativePlatformDetector.mappedFileName("7f-80 rw-p 0 00:00 0 [stack]"));
        assertNull(NativePlatformDetector.mappedFileName("7f-80 rw-p 0 00:00 0 "));
        assertNull(NativePlatformDetector.mappedFileName("7f-80 rw-p 0 00:00 0 /dir/"));
    }

    @ParameterizedTest
    @CsvSource({
        "linux, x86_64, true, 'linux-x86_64-musl,linux-x86_64'",
        "linux, aarch64, true, 'linux-aarch64-musl,linux-aarch64'",
        "linux, x86_64, false, linux-x86_64",
        // The musl flag is meaningless off Linux.
        "macos, aarch64, true, macos-aarch64",
        "windows, x86_64, false, windows-x86_64",
    })
    void candidatePlatformsPreferMuslOnlyOnMuslLinux(
            String os, String arch, boolean musl, String expected) {
        assertEquals(List.of(expected.split(",")),
                NativePlatformDetector.candidatePlatforms(os, arch, musl));
    }

    @ParameterizedTest
    @CsvSource({
        "Windows 11, windows", "Mac OS X, macos", "Darwin, macos", "Linux, linux", "FreeBSD, linux",
    })
    void detectsTheOsSegment(String osName, String expected) {
        assertEquals(expected, NativePlatformDetector.detectOs(osName));
    }

    @ParameterizedTest
    @CsvSource({"amd64, x86_64", "x86_64, x86_64", "aarch64, aarch64", "ARM64, aarch64", "riscv64, riscv64"})
    void detectsTheArchSegment(String osArch, String expected) {
        assertEquals(expected, NativePlatformDetector.detectArch(osArch));
    }

    @Test
    void hostCandidatePlatformsAppliesTheOverrideOnLinux() {
        String oldOs = System.getProperty("os.name");
        String oldArch = System.getProperty("os.arch");
        String oldLibc = System.getProperty(NativePlatformDetector.LIBC_PROPERTY);
        try {
            System.setProperty("os.name", "Linux");
            System.setProperty("os.arch", "amd64");
            System.setProperty(NativePlatformDetector.LIBC_PROPERTY, "musl");
            assertEquals(List.of("linux-x86_64-musl", "linux-x86_64"),
                    NativePlatformDetector.hostCandidatePlatforms(probe(GLIBC_MAPS, Map.of())));
            System.clearProperty(NativePlatformDetector.LIBC_PROPERTY);
            assertEquals(List.of("linux-x86_64"),
                    NativePlatformDetector.hostCandidatePlatforms(probe(GLIBC_MAPS, Map.of())));
            System.setProperty("os.name", "Windows 11");
            assertEquals(List.of("windows-x86_64"),
                    NativePlatformDetector.hostCandidatePlatforms(probe(MUSL_MAPS, Map.of())));
        } finally {
            restore("os.name", oldOs);
            restore("os.arch", oldArch);
            restore(NativePlatformDetector.LIBC_PROPERTY, oldLibc);
        }
    }

    @Test
    void hostProbeNeverThrows() {
        // Whatever the host: unreadable paths degrade to null / empty.
        NativePlatformDetector.HOST.selfMaps();
        assertTrue(NativePlatformDetector.HOST.listDir("/definitely/not/a/dir/" + System.nanoTime()).isEmpty());
    }

    @Test
    void muslResourceIsPreferredWhenBundled() {
        Set<String> bundled = Set.of("native/linux-x86_64-musl/libdemo.so", "native/linux-x86_64/libdemo.so");
        assertEquals("native/linux-x86_64-musl/libdemo.so", VesperaNativeLoader.resolveBundledResource(
                List.of("linux-x86_64-musl", "linux-x86_64"), "libdemo.so", p -> bundled.contains(p) ? stream() : null).path());
    }

    @Test
    void muslFallsBackToTheGlibcResourceWhenNoMuslOneIsBundled() {
        List<String> looked = new ArrayList<>();
        VesperaNativeLoader.BundledResource found = VesperaNativeLoader.resolveBundledResource(
                List.of("linux-aarch64-musl", "linux-aarch64"), "libdemo.so", p -> {
                    looked.add(p);
                    return p.equals("native/linux-aarch64/libdemo.so") ? stream() : null;
                });
        assertEquals("native/linux-aarch64/libdemo.so", found.path());
        assertNotNull(found.stream());
        assertEquals(List.of("native/linux-aarch64-musl/libdemo.so", "native/linux-aarch64/libdemo.so"), looked);
    }

    @Test
    void glibcNeverLooksAtTheMuslResource() {
        List<String> looked = new ArrayList<>();
        assertThrows(VesperaNativeLoader.BundledNativeAbsent.class, () ->
                VesperaNativeLoader.resolveBundledResource(List.of("linux-x86_64"), "libdemo.so", p -> {
                    looked.add(p);
                    return null;
                }));
        assertEquals(List.of("native/linux-x86_64/libdemo.so"), looked);
    }

    @Test
    void absentEverywhereNamesEveryPathTried() {
        var absent = assertThrows(VesperaNativeLoader.BundledNativeAbsent.class, () ->
                VesperaNativeLoader.resolveBundledResource(
                        List.of("linux-x86_64-musl", "linux-x86_64"), "libdemo.so", p -> null));
        assertEquals("Not found in JAR: native/linux-x86_64-musl/libdemo.so, native/linux-x86_64/libdemo.so",
                absent.getMessage());
    }

    private static InputStream stream() {
        return new ByteArrayInputStream(new byte[] {1});
    }

    private static void restore(String key, String value) {
        if (value == null) {
            System.clearProperty(key);
        } else {
            System.setProperty(key, value);
        }
    }
}
