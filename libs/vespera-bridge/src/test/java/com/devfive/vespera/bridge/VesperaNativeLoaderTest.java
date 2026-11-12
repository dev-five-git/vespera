package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.FileTime;
import java.nio.file.attribute.PosixFilePermissions;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import java.util.Random;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.Test;

class VesperaNativeLoaderTest {

    @Test
    void utilityConstructorIsPrivateButWellFormed() throws Exception {
        var constructor = VesperaNativeLoader.class.getDeclaredConstructor();
        constructor.setAccessible(true);
        assertInstanceOf(VesperaNativeLoader.class, constructor.newInstance());
    }

    @Test
    void absentBundledLibraryReportsResolvedResourcePath() {
        VesperaNativeLoader.BundledNativeAbsent error = assertThrows(
                VesperaNativeLoader.BundledNativeAbsent.class,
                () -> VesperaNativeLoader.loadBundled("absent_" + UUID.randomUUID()));

        assertTrue(error.getMessage().startsWith("Not found in JAR: native/"), error.getMessage());
        assertTrue(error.getMessage().contains(System.getProperty("os.arch").contains("64")
                ? "64"
                : System.getProperty("os.arch")), error.getMessage());
    }

    @Test
    void osArchAndLibraryMappingsCoverSupportedTriples() throws Exception {
        String oldOs = System.getProperty("os.name");
        String oldArch = System.getProperty("os.arch");
        try {
            assertDetection("Windows 11", "amd64", "windows", "x86_64", "demo.dll");
            assertDetection("Mac OS X", "arm64", "macos", "aarch64", "libdemo.dylib");
            assertDetection("Darwin", "aarch64", "macos", "aarch64", "libdemo.dylib");
            assertDetection("FreeBSD", "riscv64", "linux", "riscv64", "libdemo.so");
            assertDetection("Linux", "x86_64", "linux", "x86_64", "libdemo.so");
        } finally {
            restoreProperty("os.name", oldOs);
            restoreProperty("os.arch", oldArch);
        }
    }

    @Test
    void digestHelperReadsWholeFileAndResetsDigest() throws Exception {
        Path temp = Files.createTempDirectory("vespera-native-loader-digest-");
        try {
            byte[] content = new byte[70 * 1024];
            for (int i = 0; i < content.length; i++) {
                content[i] = (byte) i;
            }
            Path file = temp.resolve("native.bin");
            Files.write(file, content);
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            digest.update("stale".getBytes(StandardCharsets.UTF_8));

            Method method = VesperaNativeLoader.class.getDeclaredMethod(
                    "digestOfFile", Path.class, MessageDigest.class);
            method.setAccessible(true);
            byte[] actual = (byte[]) method.invoke(null, file, digest);

            assertArrayEquals(MessageDigest.getInstance("SHA-256").digest(content), actual);
        } finally {
            deleteTree(temp);
        }
    }

    @Test
    void presentBundledResourceIsExtractedVerifiedAndRejectedOnlyAtSystemLoad() throws Exception {
        Path temp = Files.createTempDirectory("vespera-native-loader-resource-");
        String oldVerify = System.getProperty("vespera.native.verifyExtractedDigest");
        String oldDir = System.getProperty(VesperaNativeLoader.NATIVE_DIR_PROPERTY);
        try {
            String os = invokeString(VesperaNativeLoader.class, "detectOs");
            String arch = invokeString(VesperaNativeLoader.class, "detectArch");
            String libraryName = "fabricated_" + UUID.randomUUID().toString().replace("-", "");
            String filename = invokeString(VesperaNativeLoader.class, "mapLibraryName", os, libraryName);
            Path resource = temp.resolve("native").resolve(os + "-" + arch).resolve(filename);
            Files.createDirectories(resource.getParent());
            Files.write(resource, "not a native library".getBytes(StandardCharsets.UTF_8));
            System.setProperty("vespera.native.verifyExtractedDigest", "true");
            Path extractDir = Files.createDirectories(temp.resolve("extract"));
            System.setProperty(VesperaNativeLoader.NATIVE_DIR_PROPERTY, extractDir.toString());

            try (ChildFirstLoader loader = new ChildFirstLoader(
                    new URL[] {temp.toUri().toURL(), mainClassesUrl()})) {
                Throwable failure = invokeChildLoadBundled(loader, libraryName);
                UnsatisfiedLinkError error = assertInstanceOf(UnsatisfiedLinkError.class, failure);
                assertTrue(error.getMessage() != null && !error.getMessage().isBlank());
            }
            // A resource that fails System.load leaves no cached copy behind.
            try (var left = Files.list(extractDir)) {
                assertEquals(0, left.count());
            }
        } finally {
            restoreProperty("vespera.native.verifyExtractedDigest", oldVerify);
            restoreProperty(VesperaNativeLoader.NATIVE_DIR_PROPERTY, oldDir);
            deleteTree(temp);
        }
    }

    @Test
    void extractionIoFailureIsWrappedWithItsCause() throws Exception {
        Path temp = Files.createTempDirectory("vespera-native-loader-io-");
        try (ChildFirstLoader loader = new ChildFirstLoader(new URL[] {mainClassesUrl()}) {
            @Override
            public InputStream getResourceAsStream(String name) {
                if (name.startsWith("native/")) {
                    return new InputStream() {
                        @Override
                        public int read() throws IOException {
                            throw new IOException("fabricated read failure");
                        }
                    };
                }
                return super.getResourceAsStream(name);
            }
        }) {
            Throwable failure = invokeChildLoadBundled(loader, "fabricated_io");
            UnsatisfiedLinkError error = assertInstanceOf(UnsatisfiedLinkError.class, failure);
            assertTrue(error.getMessage().contains("fabricated read failure"), error.getMessage());
            assertInstanceOf(IOException.class, error.getCause());
        } finally {
            deleteTree(temp);
        }
    }

    @Test
    void digestAvailabilitySizeDigestAndCleanupDecisionsAreExact() throws Exception {
        assertEquals("SHA-256", VesperaNativeLoader.messageDigest("SHA-256").getAlgorithm());
        UnsatisfiedLinkError unavailable = assertThrows(
                UnsatisfiedLinkError.class,
                () -> VesperaNativeLoader.messageDigest("fabricated-missing-digest"));
        assertTrue(unavailable.getMessage().startsWith(
                "fabricated-missing-digest unavailable for native library verification:"),
                unavailable.getMessage());

        VesperaNativeLoader.requireMatchingSize("native/demo", 4, 4);
        UnsatisfiedLinkError size = assertThrows(
                UnsatisfiedLinkError.class,
                () -> VesperaNativeLoader.requireMatchingSize("native/demo", 4, 5));
        assertEquals(
                "Native library extraction failed for native/demo: copied 4 bytes but extracted file has 5 bytes.",
                size.getMessage());

        byte[] digest = new byte[] {1, 2, 3};
        VesperaNativeLoader.requireMatchingDigest("native/demo", digest, digest.clone());
        UnsatisfiedLinkError mismatch = assertThrows(
                UnsatisfiedLinkError.class,
                () -> VesperaNativeLoader.requireMatchingDigest(
                        "native/demo", digest, new byte[] {1, 2, 4}));
        assertEquals(
                "Native library integrity check failed for native/demo: extracted file does not match the bundled resource (corrupted or modified extraction).",
                mismatch.getMessage());

        Path temp = Files.createTempDirectory("vespera-native-loader-cleanup-");
        try {
            Path file = Files.writeString(temp.resolve("native.bin"), "x");
            VesperaNativeLoader.deleteAfterFailedLoad(file, Files::deleteIfExists);
            assertTrue(Files.notExists(file));
            Files.writeString(file, "still present");
            VesperaNativeLoader.deleteAfterFailedLoad(
                    file, path -> { throw new IOException("locked"); });
            assertEquals("still present", Files.readString(file));
        } finally {
            deleteTree(temp);
        }
    }

    // ── Content-addressed extraction ───────────────────────────────────────

    @Test
    void baseDirPrefersPropertyThenEnvThenPerUserTmpdir() {
        Map<String, String> props = new HashMap<>();
        Map<String, String> env = new HashMap<>();
        props.put("java.io.tmpdir", "tmp-root");
        props.put("user.name", "al ice/..\\x");
        assertEquals(Path.of("tmp-root", "vespera-al_ice_.._x"),
                VesperaNativeLoader.resolveBaseDir(props::get, env::get));
        env.put(VesperaNativeLoader.NATIVE_DIR_ENV, "  ");
        assertEquals(Path.of("tmp-root", "vespera-al_ice_.._x"),
                VesperaNativeLoader.resolveBaseDir(props::get, env::get));
        env.put(VesperaNativeLoader.NATIVE_DIR_ENV, "env-root");
        assertEquals(Path.of("env-root"),
                VesperaNativeLoader.resolveBaseDir(props::get, env::get));
        props.put(VesperaNativeLoader.NATIVE_DIR_PROPERTY, " prop-root ");
        assertEquals(Path.of("prop-root"),
                VesperaNativeLoader.resolveBaseDir(props::get, env::get));
    }

    @Test
    void userNameSanitizationNeverYieldsAnEmptyOrDotComponent() {
        assertEquals("user", VesperaNativeLoader.sanitizeUserName(null));
        assertEquals("user", VesperaNativeLoader.sanitizeUserName("  "));
        assertEquals("user", VesperaNativeLoader.sanitizeUserName(".."));
        assertEquals("svc-app_1.x", VesperaNativeLoader.sanitizeUserName("svc-app_1.x"));
        assertEquals("DOMAIN_bob", VesperaNativeLoader.sanitizeUserName("DOMAIN\\bob"));
    }

    // ── POSIX private-directory enforcement ────────────────────────────────

    private static final byte[] LIB = "pretend cdylib".getBytes(StandardCharsets.UTF_8);

    private static Path extractLib(Path base) throws IOException {
        return VesperaNativeLoader.extract("native/x/libdemo.so", "libdemo.so", base,
                () -> new ByteArrayInputStream(LIB));
    }

    private static boolean posix() {
        return FileSystems.getDefault().supportedFileAttributeViews().contains("posix");
    }

    private static void deleteFallback(Path extracted, Path base) throws IOException {
        if (!extracted.startsWith(base)) {
            deleteTree(extracted.getParent());
        }
    }

    @Test
    void privateDirectoriesAreCreatedOwnerOnlyAndTheLibraryReadOnly() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Path root = Files.createTempDirectory("vespera-native-private-");
        try {
            Path base = root.resolve("nested").resolve("base");
            Path lib = extractLib(base);
            assertTrue(lib.startsWith(base), "a private base dir is used directly");
            assertEquals("rwx------", PosixFilePermissions.toString(
                    Files.getPosixFilePermissions(base)));
            assertEquals("rwx------", PosixFilePermissions.toString(
                    Files.getPosixFilePermissions(lib.getParent())));
            assertEquals("r-x------", PosixFilePermissions.toString(
                    Files.getPosixFilePermissions(lib)));
            FileTime stamp = FileTime.fromMillis(1_000_000L);
            Files.setLastModifiedTime(lib, stamp);
            assertEquals(lib, extractLib(base), "a verified-private copy is reused");
            assertEquals(stamp, Files.getLastModifiedTime(lib));
        } finally {
            deleteTree(root);
        }
    }

    @Test
    void groupOrOtherWritableBaseDirFallsBackToAFreshPrivateDir() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Path base = Files.createTempDirectory("vespera-native-shared-");
        Path first = null;
        Path second = null;
        try {
            Files.setPosixFilePermissions(base, PosixFilePermissions.fromString("rwxrwxrwx"));
            assertThrows(VesperaNativeLoader.UnsafeDirectoryException.class,
                    () -> VesperaNativeLoader.requirePrivateDirectory(base));
            first = extractLib(base);
            second = extractLib(base);
            assertFalse(first.startsWith(base), "a shared base dir is never used");
            assertNotEquals(first.getParent(), second.getParent(), "no reuse across calls");
            assertArrayEquals(LIB, Files.readAllBytes(first));
            assertEquals("rwx------", PosixFilePermissions.toString(
                    Files.getPosixFilePermissions(first.getParent())));
        } finally {
            if (first != null) {
                deleteFallback(first, base);
            }
            if (second != null) {
                deleteFallback(second, base);
            }
            deleteTree(base);
        }
    }

    @Test
    void plantedHashDirWithLooseModeFallsBack() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Path base = Files.createTempDirectory("vespera-native-planted-");
        Path lib = null;
        try {
            String hex = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(LIB));
            Path planted = Files.createDirectory(base.resolve("vespera-native-" + hex.substring(0, 16)));
            Files.setPosixFilePermissions(planted, PosixFilePermissions.fromString("rwxrwxrwx"));
            Files.write(planted.resolve("libdemo.so"), LIB);
            lib = extractLib(base);
            assertFalse(lib.startsWith(base), "a loose hash dir is not trusted even if bytes match");
            assertArrayEquals(LIB, Files.readAllBytes(lib));
        } finally {
            if (lib != null) {
                deleteFallback(lib, base);
            }
            deleteTree(base);
        }
    }

    @Test
    void symlinkedBaseDirFallsBack() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Path root = Files.createTempDirectory("vespera-native-symlink-");
        Path lib = null;
        try {
            Path real = Files.createDirectory(root.resolve("real"),
                    PosixFilePermissions.asFileAttribute(PosixFilePermissions.fromString("rwx------")));
            Path link = Files.createSymbolicLink(root.resolve("link"), real);
            assertThrows(VesperaNativeLoader.UnsafeDirectoryException.class,
                    () -> VesperaNativeLoader.requirePrivateDirectory(link));
            lib = extractLib(link);
            assertFalse(lib.startsWith(link));
            assertFalse(lib.startsWith(real));
            try (var left = Files.list(real)) {
                assertEquals(0, left.count(), "nothing is written through the symlink");
            }
        } finally {
            if (lib != null) {
                deleteFallback(lib, root);
            }
            deleteTree(root);
        }
    }

    @Test
    void extractionIsContentAddressedAndReusedAcrossCalls() throws Exception {
        Path base = Files.createTempDirectory("vespera-native-cas-");
        try {
            byte[] content = "pretend cdylib v1".getBytes(StandardCharsets.UTF_8);
            AtomicInteger opens = new AtomicInteger();
            VesperaNativeLoader.ResourceOpener opener = () -> {
                opens.incrementAndGet();
                return new ByteArrayInputStream(content);
            };

            Path first = VesperaNativeLoader.extract("native/x/demo.dll", "demo.dll", base, opener);
            String hex = HexFormat.of().formatHex(
                    MessageDigest.getInstance("SHA-256").digest(content));
            assertEquals(base.resolve("vespera-native-" + hex.substring(0, 16)).resolve("demo.dll"),
                    first);
            assertArrayEquals(content, Files.readAllBytes(first));
            assertEquals(2, opens.get(), "fresh extraction hashes, then copies");
            assertEquals(1, countEntries(first.getParent()), "no temp file left behind");

            FileTime stamp = FileTime.fromMillis(1_000_000L);
            Files.setLastModifiedTime(first, stamp);
            Path second = VesperaNativeLoader.extract("native/x/demo.dll", "demo.dll", base, opener);
            assertEquals(first, second);
            assertEquals(3, opens.get(), "a verified copy is reused after hashing only");
            assertEquals(stamp, Files.getLastModifiedTime(second), "reused file is not rewritten");

            byte[] other = "pretend cdylib v2".getBytes(StandardCharsets.UTF_8);
            Path third = VesperaNativeLoader.extract("native/x/demo.dll", "demo.dll", base,
                    () -> new ByteArrayInputStream(other));
            assertNotEquals(first.getParent(), third.getParent(),
                    "different content gets a different directory");
        } finally {
            deleteTree(base);
        }
    }

    @Test
    void corruptedCachedCopyIsReplaced() throws Exception {
        Path base = Files.createTempDirectory("vespera-native-corrupt-");
        try {
            byte[] content = "good bytes".getBytes(StandardCharsets.UTF_8);
            Path target = VesperaNativeLoader.extract("native/x/demo.dll", "demo.dll", base,
                    () -> new ByteArrayInputStream(content));
            // POSIX publishes the library owner read/execute only.
            assertTrue(target.toFile().setWritable(true, true) || !posix());
            Files.writeString(target, "tampered");

            Path again = VesperaNativeLoader.extract("native/x/demo.dll", "demo.dll", base,
                    () -> new ByteArrayInputStream(content));

            assertEquals(target, again);
            assertArrayEquals(content, Files.readAllBytes(again));
        } finally {
            deleteTree(base);
        }
    }

    @Test
    void concurrentExtractionsConvergeOnOneVerifiedFile() throws Exception {
        Path base = Files.createTempDirectory("vespera-native-race-");
        ExecutorService pool = Executors.newFixedThreadPool(8);
        try {
            byte[] content = new byte[512 * 1024];
            new Random(42).nextBytes(content);
            CountDownLatch start = new CountDownLatch(1);
            List<Future<Path>> results = new ArrayList<>();
            for (int i = 0; i < 8; i++) {
                results.add(pool.submit(() -> {
                    start.await();
                    return VesperaNativeLoader.extract("native/x/demo.dll", "demo.dll", base,
                            () -> new ByteArrayInputStream(content));
                }));
            }
            start.countDown();
            Path expected = results.get(0).get();
            for (Future<Path> result : results) {
                assertEquals(expected, result.get());
            }
            assertArrayEquals(content, Files.readAllBytes(expected));
            assertEquals(1, countEntries(expected.getParent()), "losers clean up their temp files");
        } finally {
            pool.shutdownNow();
            deleteTree(base);
        }
    }

    @Test
    void unusableBaseDirSurfacesIoExceptionOrFallsBackOnPosix() throws Exception {
        Path base = Files.createTempDirectory("vespera-native-unusable-");
        try {
            Path file = Files.writeString(base.resolve("not-a-dir"), "x");
            // POSIX and Windows alike: anything short of a verified-private
            // dir (here: not a directory at all) degrades to a fresh temp dir.
            Path lib = VesperaNativeLoader.extract("native/x/demo.dll", "demo.dll", file,
                    () -> new ByteArrayInputStream(new byte[] {1}));
            assertFalse(lib.startsWith(base));
            deleteFallback(lib, base);
        } finally {
            deleteTree(base);
        }
    }

    // ── Windows (ACL) directory checks ────────────────────────────────────

    private record Principal(String getName) implements java.nio.file.attribute.UserPrincipal {}

    private static java.nio.file.attribute.AclEntry allow(
            java.nio.file.attribute.UserPrincipal who,
            java.util.Set<java.nio.file.attribute.AclEntryFlag> flags,
            java.nio.file.attribute.AclEntryPermission... perms) {
        return java.nio.file.attribute.AclEntry.newBuilder()
                .setType(java.nio.file.attribute.AclEntryType.ALLOW)
                .setPrincipal(who)
                .setFlags(flags)
                .setPermissions(perms)
                .build();
    }

    @Test
    void aclProblemFlagsWriteAccessForUntrustedPrincipalsOnly() {
        var me = new Principal("HOST\\me");
        var users = new Principal("BUILTIN\\Users");
        var read = java.nio.file.attribute.AclEntryPermission.READ_DATA;
        var write = java.nio.file.attribute.AclEntryPermission.WRITE_DATA;
        var append = java.nio.file.attribute.AclEntryPermission.APPEND_DATA;
        java.util.Set<java.nio.file.attribute.AclEntryFlag> none = java.util.Set.of();
        java.util.function.Predicate<java.nio.file.attribute.UserPrincipal> trusted =
                p -> p.equals(me) || VesperaNativeLoader.isWellKnownAdmin(p);

        assertNull(VesperaNativeLoader.aclProblem(List.of(
                allow(me, none, write),
                allow(new Principal("NT AUTHORITY\\SYSTEM"), none, write),
                allow(new Principal("BUILTIN\\Administrators"), none, write),
                allow(users, none, read),
                // Inherit-only (applies to children only, e.g. CREATOR OWNER).
                allow(new Principal("CREATOR OWNER"), java.util.Set.of(
                        java.nio.file.attribute.AclEntryFlag.INHERIT_ONLY), write),
                // An unresolvable SID (deleted / sandbox account).
                allow(new Principal("S-1-5-21-1-2-3-1001"), none, write)), trusted));
        String problem = VesperaNativeLoader.aclProblem(List.of(
                allow(me, none, write), allow(users, none, read, append)), trusted);
        assertTrue(problem != null && problem.contains("BUILTIN\\Users"), problem);
        assertTrue(VesperaNativeLoader.aclProblem(List.of(java.nio.file.attribute.AclEntry.newBuilder()
                .setType(java.nio.file.attribute.AclEntryType.DENY).setPrincipal(users)
                .setPermissions(write).build()), trusted) == null, "DENY entries are ignored");
    }

    @Test
    @org.junit.jupiter.api.condition.EnabledOnOs(org.junit.jupiter.api.condition.OS.WINDOWS)
    void windowsReusesAnOwnedPrivateDirAndFallsBackWhenOthersMayWrite() throws Exception {
        Path base = Files.createTempDirectory("vespera-native-acl-");
        try {
            VesperaNativeLoader.ExtractionDir owned = VesperaNativeLoader.prepareDir(base, "vespera-native-abc");
            assertTrue(owned.reusable(), "a fresh per-user temp dir is private");
            assertEquals(base.resolve("vespera-native-abc"), owned.dir());

            // Plant: grant Everyone write on the predictable hash directory.
            var view = Files.getFileAttributeView(owned.dir(),
                    java.nio.file.attribute.AclFileAttributeView.class);
            java.nio.file.attribute.UserPrincipal everyone;
            try {
                everyone = base.getFileSystem().getUserPrincipalLookupService()
                        .lookupPrincipalByGroupName("Everyone");
            } catch (IOException localised) {
                org.junit.jupiter.api.Assumptions.abort("no 'Everyone' group on this Windows locale");
                return;
            }
            List<java.nio.file.attribute.AclEntry> acl = new ArrayList<>(view.getAcl());
            acl.add(0, allow(everyone, java.util.Set.of(),
                    java.nio.file.attribute.AclEntryPermission.WRITE_DATA,
                    java.nio.file.attribute.AclEntryPermission.APPEND_DATA));
            view.setAcl(acl);

            VesperaNativeLoader.ExtractionDir fallback =
                    VesperaNativeLoader.prepareDir(base, "vespera-native-abc");
            assertFalse(fallback.reusable());
            assertFalse(fallback.dir().startsWith(base), "a fresh, randomly named directory");
            Files.deleteIfExists(fallback.dir());
        } finally {
            deleteTree(base);
        }
    }

    @Test
    void legacyCleanupDeletesOnlyOldLegacyNamedFiles() throws Exception {
        Path tmp = Files.createTempDirectory("vespera-native-legacy-");
        String oldFlag = System.getProperty("vespera.native.cleanupLegacy");
        try {
            long now = System.currentTimeMillis();
            FileTime old = FileTime.fromMillis(now - VesperaNativeLoader.LEGACY_MIN_AGE_MILLIS - 1000);
            Path oldDll = Files.writeString(tmp.resolve("vespera-123456789.dll"), "x");
            Path oldSo = Files.writeString(tmp.resolve("vespera-42.so"), "x");
            Path youngDll = Files.writeString(tmp.resolve("vespera-987.dll"), "x");
            Path foreign = Files.writeString(tmp.resolve("vespera-notes.txt"), "x");
            Path casDir = Files.createDirectories(tmp.resolve("vespera-native-0123456789abcdef"));
            for (Path p : new Path[] {oldDll, oldSo, foreign, casDir}) {
                Files.setLastModifiedTime(p, old);
            }

            System.setProperty("vespera.native.cleanupLegacy", "false");
            assertEquals(0, VesperaNativeLoader.cleanupLegacyExtractions(tmp, now));
            System.clearProperty("vespera.native.cleanupLegacy");

            assertEquals(2, VesperaNativeLoader.cleanupLegacyExtractions(tmp, now));
            assertTrue(Files.notExists(oldDll));
            assertTrue(Files.notExists(oldSo));
            assertTrue(Files.exists(youngDll));
            assertTrue(Files.exists(foreign));
            assertTrue(Files.isDirectory(casDir));
            assertEquals(0, VesperaNativeLoader.cleanupLegacyExtractions(tmp.resolve("missing"), now));
        } finally {
            restoreProperty("vespera.native.cleanupLegacy", oldFlag);
            deleteTree(tmp);
        }
    }

    private static long countEntries(Path dir) throws IOException {
        try (var entries = Files.list(dir)) {
            return entries.count();
        }
    }

    private static void assertDetection(
            String osProperty,
            String archProperty,
            String expectedOs,
            String expectedArch,
            String expectedFilename) throws Exception {
        System.setProperty("os.name", osProperty);
        System.setProperty("os.arch", archProperty);
        assertEquals(expectedOs, invokeString(VesperaNativeLoader.class, "detectOs"));
        assertEquals(expectedArch, invokeString(VesperaNativeLoader.class, "detectArch"));
        assertEquals(expectedFilename,
                invokeString(VesperaNativeLoader.class, "mapLibraryName", expectedOs, "demo"));
    }

    private static String invokeString(Class<?> type, String name, Object... args) throws Exception {
        Class<?>[] parameterTypes = new Class<?>[args.length];
        for (int i = 0; i < args.length; i++) {
            parameterTypes[i] = String.class;
        }
        Method method = type.getDeclaredMethod(name, parameterTypes);
        method.setAccessible(true);
        return (String) method.invoke(null, args);
    }

    private static Throwable invokeChildLoadBundled(ClassLoader loader, String libraryName)
            throws Exception {
        Class<?> childLoader = Class.forName(
                "com.devfive.vespera.bridge.VesperaNativeLoader", true, loader);
        Method loadBundled = childLoader.getDeclaredMethod("loadBundled", String.class);
        loadBundled.setAccessible(true);
        InvocationTargetException invocation = assertThrows(
                InvocationTargetException.class,
                () -> loadBundled.invoke(null, libraryName));
        return invocation.getCause();
    }

    private static URL mainClassesUrl() {
        return VesperaBridge.class.getProtectionDomain().getCodeSource().getLocation();
    }

    private static void restoreProperty(String name, String value) {
        if (value == null) {
            System.clearProperty(name);
        } else {
            System.setProperty(name, value);
        }
    }

    private static void deleteTree(Path root) throws IOException {
        if (!Files.exists(root)) {
            return;
        }
        try (var paths = Files.walk(root)) {
            for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
                Files.delete(path);
            }
        }
    }

    private static class ChildFirstLoader extends URLClassLoader {
        ChildFirstLoader(URL[] urls) {
            super(urls, VesperaNativeLoaderTest.class.getClassLoader());
        }

        @Override
        protected Class<?> loadClass(String name, boolean resolve) throws ClassNotFoundException {
            if (name.equals("com.devfive.vespera.bridge.VesperaBridge")
                    || name.startsWith("com.devfive.vespera.bridge.VesperaNativeLoader")) {
                synchronized (getClassLoadingLock(name)) {
                    Class<?> loaded = findLoadedClass(name);
                    if (loaded == null) {
                        loaded = findClass(name);
                    }
                    if (resolve) {
                        resolveClass(loaded);
                    }
                    return loaded;
                }
            }
            return super.loadClass(name, resolve);
        }
    }
}
