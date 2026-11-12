package com.devfive.vespera.bridge;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.DirectoryStream;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.nio.file.attribute.PosixFileAttributes;
import java.nio.file.attribute.PosixFilePermission;
import java.nio.file.attribute.PosixFilePermissions;
import java.nio.file.attribute.UserPrincipal;
import java.security.DigestInputStream;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HexFormat;
import java.util.Set;
import java.util.function.UnaryOperator;
import java.util.regex.Pattern;

/**
 * Native library lookup/extraction helpers for {@link VesperaBridge}.
 *
 * <p><strong>Content-addressed extraction.</strong> The bundled cdylib is
 * extracted to
 * {@code <base>/vespera-native-<sha256-prefix>/<libname>}, where
 * {@code <base>} is the first of: system property {@code vespera.native.dir},
 * environment variable {@code VESPERA_NATIVE_DIR}, or the per-user default
 * {@code <java.io.tmpdir>/vespera-<user.name>}.
 * A JVM restart reuses the existing file when its SHA-256 matches the bundled
 * resource (no new copy per start, nothing to clean up on exit — which
 * {@code deleteOnExit} never managed for a loaded Windows DLL). Point
 * {@code vespera.native.dir} at an exec-permitted directory when
 * {@code /tmp} is mounted {@code noexec}.
 *
 * <p><strong>Private directories (POSIX).</strong> Both the base and the
 * hash directory are created {@code 0700} and then verified — not a
 * symbolic link, owned by the current user, not group/other-writable —
 * before a cached copy is trusted, so another local user can neither plant
 * nor swap the library between the digest check and {@code System.load}.
 * The extracted file is made owner read/execute only ({@code 0500}).  When
 * verification fails (a squatted, shared or symlinked directory) the
 * library is extracted into a fresh private
 * {@code Files.createTempDirectory("vespera-native-")} for this JVM only,
 * with no reuse, and a warning is logged.  On file systems without POSIX
 * attributes (Windows) the directories are used as-is; the default base
 * there lives under the already per-user {@code java.io.tmpdir}.
 *
 * <p>A fresh extraction writes a temp file inside the target directory and
 * publishes it with an atomic rename, so concurrent JVMs either see no file
 * or a complete one; a JVM that loses the rename race verifies and reuses
 * the winner's file.
 */
final class VesperaNativeLoader {

    private static final System.Logger LOG = System.getLogger(VesperaBridge.class.getName());

    private static final Set<PosixFilePermission> OWNER_ONLY_DIR =
            PosixFilePermissions.fromString("rwx------");
    private static final Set<PosixFilePermission> OWNER_READ_EXEC_FILE =
            PosixFilePermissions.fromString("r-x------");

    private VesperaNativeLoader() {}

    /** System property selecting the extraction base directory. */
    static final String NATIVE_DIR_PROPERTY = "vespera.native.dir";
    /** Environment variable selecting the extraction base directory. */
    static final String NATIVE_DIR_ENV = "VESPERA_NATIVE_DIR";
    /** Number of hex digits of the SHA-256 used in the directory name. */
    static final int DIGEST_PREFIX_HEX = 16;
    /**
     * Legacy (pre content-addressing) extractions were
     * {@code Files.createTempFile("vespera-", suffix)} in {@code java.io.tmpdir}.
     */
    private static final Pattern LEGACY_EXTRACTION =
            Pattern.compile("vespera-\\d+\\.(dll|so|dylib)");
    /** Legacy files younger than this may still be mid-load by an old JVM. */
    static final long LEGACY_MIN_AGE_MILLIS = 60L * 60L * 1000L;

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

    /** Supplies the bundled resource stream (re-openable: read up to twice). */
    @FunctionalInterface
    interface ResourceOpener {
        InputStream open() throws IOException;
    }

    static void loadBundled(String libraryName) {
        String os = detectOs();
        String arch = detectArch();
        String filename = mapLibraryName(os, libraryName);
        String resourcePath = "native/" + os + "-" + arch + "/" + filename;
        ClassLoader loader = VesperaBridge.class.getClassLoader();

        InputStream probe = loader.getResourceAsStream(resourcePath);
        if (probe == null) {
            throw new BundledNativeAbsent("Not found in JAR: " + resourcePath);
        }
        InputStream[] first = {probe};
        ResourceOpener opener = () -> {
            InputStream in = first[0];
            first[0] = null;
            if (in == null) {
                in = loader.getResourceAsStream(resourcePath);
            }
            if (in == null) {
                throw new IOException("bundled resource disappeared: " + resourcePath);
            }
            return in;
        };
        Path baseDir = resolveBaseDir(System::getProperty, System::getenv);
        Path library;
        try {
            library = extract(resourcePath, filename, baseDir, opener);
        } catch (IOException e) {
            UnsatisfiedLinkError ule = new UnsatisfiedLinkError("Extract failed: " + e.getMessage()
                    + " (extraction dir " + baseDir + "; override with -D" + NATIVE_DIR_PROPERTY
                    + " or " + NATIVE_DIR_ENV + ")");
            ule.initCause(e);
            throw ule;
        } finally {
            closeQuietly(first[0]);
        }
        try {
            System.load(library.toAbsolutePath().toString());
        } catch (UnsatisfiedLinkError loadFailure) {
            // The bytes match the bundled resource, so the resource itself is
            // unloadable here; don't leave a dead cached copy behind.
            deleteAfterFailedLoad(library, Files::deleteIfExists);
            deleteAfterFailedLoad(library.getParent(), Files::deleteIfExists);
            throw loadFailure;
        }
        cleanupLegacyExtractions(
                Paths.get(System.getProperty("java.io.tmpdir")), System.currentTimeMillis());
    }

    private static void closeQuietly(InputStream in) {
        if (in != null) {
            try {
                in.close();
            } catch (IOException ignored) {
                // nothing to report
            }
        }
    }

    /**
     * {@code vespera.native.dir} → {@code VESPERA_NATIVE_DIR} →
     * {@code <java.io.tmpdir>/vespera-<sanitized user.name>}; blank values
     * are ignored.
     */
    static Path resolveBaseDir(UnaryOperator<String> properties, UnaryOperator<String> env) {
        String configured = properties.apply(NATIVE_DIR_PROPERTY);
        if (configured == null || configured.isBlank()) {
            configured = env.apply(NATIVE_DIR_ENV);
        }
        if (configured != null && !configured.isBlank()) {
            return Paths.get(configured.trim());
        }
        return Paths.get(properties.apply("java.io.tmpdir").trim())
                .resolve("vespera-" + sanitizeUserName(properties.apply("user.name")));
    }

    /** File-name-safe form of {@code user.name} ({@code [A-Za-z0-9._-]}, never empty). */
    static String sanitizeUserName(String userName) {
        if (userName == null || userName.isBlank()) {
            return "user";
        }
        String safe = userName.trim().replaceAll("[^A-Za-z0-9._-]", "_");
        // "." / ".." must never become a path component.
        return safe.chars().allMatch(c -> c == '.') ? "user" : safe;
    }

    /** Where to extract and whether a cached copy found there may be trusted. */
    record ExtractionDir(Path dir, boolean reusable) {}

    /**
     * Thrown when a directory exists but is not private to the current user
     * (symbolic link, foreign owner, group/other-writable).
     */
    static final class UnsafeDirectoryException extends IOException {
        UnsafeDirectoryException(String message) {
            super(message);
        }
    }

    static boolean posixSupported(Path dir) {
        return dir.getFileSystem().supportedFileAttributeViews().contains("posix");
    }

    /**
     * Create (if needed) and verify {@code baseDir/hashDirName}.  On POSIX a
     * failure to obtain a verified-private directory falls back to a fresh
     * private temp directory for this JVM ({@code reusable == false}).
     */
    static ExtractionDir prepareDir(Path baseDir, String hashDirName) throws IOException {
        if (!posixSupported(baseDir)) {
            Path dir = baseDir.resolve(hashDirName);
            Files.createDirectories(dir);
            return new ExtractionDir(dir, true);
        }
        try {
            createPrivateDirectories(baseDir);
            requirePrivateDirectory(baseDir);
            Path dir = baseDir.resolve(hashDirName);
            try {
                Files.createDirectory(dir, PosixFilePermissions.asFileAttribute(OWNER_ONLY_DIR));
            } catch (FileAlreadyExistsException exists) {
                // Verified below.
            }
            requirePrivateDirectory(dir);
            return new ExtractionDir(dir, true);
        } catch (IOException | SecurityException notPrivate) {
            Path fresh = Files.createTempDirectory(
                    "vespera-native-", PosixFilePermissions.asFileAttribute(OWNER_ONLY_DIR));
            fresh.toFile().deleteOnExit();
            LOG.log(System.Logger.Level.WARNING, "vespera: native extraction directory "
                    + baseDir.resolve(hashDirName) + " is not private to this user ("
                    + notPrivate.getMessage() + "); extracting into " + fresh
                    + " for this JVM only (no reuse). Point -D" + NATIVE_DIR_PROPERTY
                    + " at a directory owned by this user with mode 0700.");
            return new ExtractionDir(fresh, false);
        }
    }

    private static void createPrivateDirectories(Path dir) throws IOException {
        if (Files.exists(dir, LinkOption.NOFOLLOW_LINKS)) {
            return;
        }
        Path parent = dir.toAbsolutePath().getParent();
        if (parent != null && !Files.exists(parent)) {
            Files.createDirectories(
                    parent, PosixFilePermissions.asFileAttribute(OWNER_ONLY_DIR));
        }
        try {
            Files.createDirectory(dir, PosixFilePermissions.asFileAttribute(OWNER_ONLY_DIR));
        } catch (FileAlreadyExistsException raced) {
            // Verified by the caller.
        }
    }

    /**
     * Require {@code dir} to be a real directory (not a symbolic link) owned
     * by the current user and not writable by group or others.
     */
    static void requirePrivateDirectory(Path dir) throws IOException {
        PosixFileAttributes attrs =
                Files.readAttributes(dir, PosixFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
        if (attrs.isSymbolicLink() || !attrs.isDirectory()) {
            throw new UnsafeDirectoryException(dir + " is a symbolic link or not a directory");
        }
        UserPrincipal me = currentUser();
        if (!attrs.owner().equals(me)) {
            throw new UnsafeDirectoryException(
                    dir + " is owned by " + attrs.owner().getName() + ", not " + me.getName());
        }
        Set<PosixFilePermission> perms = attrs.permissions();
        if (perms.contains(PosixFilePermission.GROUP_WRITE)
                || perms.contains(PosixFilePermission.OTHERS_WRITE)) {
            throw new UnsafeDirectoryException(dir + " is group/other-writable ("
                    + PosixFilePermissions.toString(perms) + ")");
        }
    }

    private static volatile UserPrincipal cachedCurrentUser;

    /**
     * The current OS user: the owner of a freshly created temp file (robust
     * where {@code user.name} has no passwd entry, e.g. containers).
     */
    private static UserPrincipal currentUser() throws IOException {
        UserPrincipal cached = cachedCurrentUser;
        if (cached != null) {
            return cached;
        }
        Path probe = Files.createTempFile("vespera-owner-", ".probe");
        try {
            cached = Files.getOwner(probe, LinkOption.NOFOLLOW_LINKS);
        } finally {
            Files.deleteIfExists(probe);
        }
        cachedCurrentUser = cached;
        return cached;
    }

    /**
     * Extract (or reuse) the resource at
     * {@code baseDir/vespera-native-<sha256-prefix>/filename} and return that
     * path. Reads the resource once to hash it; reads it a second time only
     * when no verified copy exists yet.
     */
    static Path extract(String resourcePath, String filename, Path baseDir, ResourceOpener opener)
            throws IOException {
        MessageDigest digest = messageDigest("SHA-256");
        byte[] resourceDigest;
        long resourceSize;
        try (InputStream in = opener.open()) {
            resourceSize = digestStream(in, digest);
            resourceDigest = digest.digest();
        }
        String hex = HexFormat.of().formatHex(resourceDigest);
        ExtractionDir prepared =
                prepareDir(baseDir, "vespera-native-" + hex.substring(0, DIGEST_PREFIX_HEX));
        Path dir = prepared.dir();
        Path target = dir.resolve(filename);

        // Only a verified-private directory may serve a cached copy: nobody
        // but this user can replace the file between this check and load.
        if (prepared.reusable() && isVerifiedCopy(target, resourceDigest, digest)) {
            return target;
        }
        if (!prepared.reusable()) {
            target.toFile().deleteOnExit();
        }
        Path temp = Files.createTempFile(dir, filename + ".", ".tmp");
        boolean published = false;
        try {
            long copiedBytes;
            try (InputStream in = opener.open();
                 DigestInputStream din = new DigestInputStream(in, digest)) {
                digest.reset();
                copiedBytes = Files.copy(din, temp, StandardCopyOption.REPLACE_EXISTING);
            }
            requireMatchingSize(resourcePath, resourceSize, copiedBytes);
            requireMatchingSize(resourcePath, copiedBytes, Files.size(temp));
            requireMatchingDigest(resourcePath, resourceDigest, digest.digest());
            if (Boolean.getBoolean("vespera.native.verifyExtractedDigest")) {
                requireMatchingDigest(resourcePath, resourceDigest, digestOfFile(temp, digest));
            }
            if (posixSupported(temp)) {
                // Owner read/execute only: not even this user's process can
                // rewrite the published library in place.
                Files.setPosixFilePermissions(temp, OWNER_READ_EXEC_FILE);
            }
            published = publish(temp, target, resourceDigest, digest);
            return target;
        } finally {
            if (!published) {
                deleteAfterFailedLoad(temp, Files::deleteIfExists);
            }
        }
    }

    /**
     * Atomically rename {@code temp} onto {@code target}. Returns {@code true}
     * when {@code temp} was consumed by the rename, {@code false} when a
     * concurrent JVM already published a verified copy (caller deletes temp).
     */
    private static boolean publish(Path temp, Path target, byte[] expected, MessageDigest digest)
            throws IOException {
        try {
            moveAtomically(temp, target);
            return true;
        } catch (IOException moveFailure) {
            // Lost the race (target appeared) or Windows refuses to replace a
            // DLL another JVM has loaded: reuse that file if it is intact.
            if (isVerifiedCopy(target, expected, digest)) {
                return false;
            }
            throw moveFailure;
        }
    }

    private static void moveAtomically(Path source, Path target) throws IOException {
        try {
            Files.move(source, target,
                    StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING);
        } catch (AtomicMoveNotSupportedException unsupported) {
            Files.move(source, target, StandardCopyOption.REPLACE_EXISTING);
        }
    }

    /** Whether {@code file} exists and its SHA-256 equals {@code expected}. */
    static boolean isVerifiedCopy(Path file, byte[] expected, MessageDigest digest) {
        if (!Files.isRegularFile(file, LinkOption.NOFOLLOW_LINKS)) {
            return false;
        }
        try {
            return MessageDigest.isEqual(expected, digestOfFile(file, digest));
        } catch (IOException unreadable) {
            return false;
        }
    }

    /**
     * Best-effort removal of pre content-addressing extractions
     * ({@code vespera-<digits>.(dll|so|dylib)} directly in {@code tmpDir})
     * older than {@link #LEGACY_MIN_AGE_MILLIS}. A file another process
     * still has loaded either cannot be deleted (Windows — the failure is
     * ignored) or stays mapped by that process after unlinking (POSIX); the
     * age guard keeps an old JVM that is extracting right now from losing its
     * file before {@code System.load}. Disable with
     * {@code -Dvespera.native.cleanupLegacy=false}. Returns the delete count.
     */
    static int cleanupLegacyExtractions(Path tmpDir, long nowMillis) {
        if ("false".equalsIgnoreCase(System.getProperty("vespera.native.cleanupLegacy"))) {
            return 0;
        }
        int deleted = 0;
        try (DirectoryStream<Path> files = Files.newDirectoryStream(tmpDir, "vespera-*")) {
            for (Path file : files) {
                try {
                    if (LEGACY_EXTRACTION.matcher(file.getFileName().toString()).matches()
                            && Files.isRegularFile(file)
                            && nowMillis - Files.getLastModifiedTime(file).toMillis()
                                    >= LEGACY_MIN_AGE_MILLIS
                            && Files.deleteIfExists(file)) {
                        deleted++;
                    }
                } catch (IOException | SecurityException inUseOrDenied) {
                    // In use by another process / not ours: leave it.
                }
            }
        } catch (IOException | SecurityException unreadableDir) {
            // Cleanup is purely opportunistic.
        }
        return deleted;
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

    private static long digestStream(InputStream in, MessageDigest digest) throws IOException {
        digest.reset();
        byte[] buf = new byte[64 * 1024];
        long total = 0;
        int n;
        while ((n = in.read(buf)) != -1) {
            digest.update(buf, 0, n);
            total += n;
        }
        return total;
    }

    private static byte[] digestOfFile(Path file, MessageDigest digest) throws IOException {
        try (InputStream fin = Files.newInputStream(file)) {
            digestStream(fin, digest);
        }
        return digest.digest();
    }

    private static String detectOs() {
        String os = System.getProperty("os.name", "").toLowerCase();
        // "darwin" MUST be tested before "win": the string "darwin" contains
        // the substring "win", so the opposite order sends a Darwin-reporting
        // JVM down the Windows path and makes it look for a .dll.
        if (os.contains("mac") || os.contains("darwin")) return "macos";
        if (os.contains("win")) return "windows";
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
