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
import java.nio.file.attribute.AclEntry;
import java.nio.file.attribute.AclEntryFlag;
import java.nio.file.attribute.AclEntryPermission;
import java.nio.file.attribute.AclEntryType;
import java.nio.file.attribute.AclFileAttributeView;
import java.nio.file.attribute.BasicFileAttributes;
import java.nio.file.attribute.PosixFileAttributes;
import java.nio.file.attribute.PosixFilePermission;
import java.nio.file.attribute.PosixFilePermissions;
import java.nio.file.attribute.UserPrincipal;
import java.security.DigestInputStream;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.EnumSet;
import java.util.HashSet;
import java.util.HexFormat;
import java.util.List;
import java.util.Set;
import java.util.function.Function;
import java.util.function.Predicate;
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
 * attributes (Windows) the same guarantee comes from the owner and the ACL
 * (see {@link #prepareAclDir}); a directory that fails it — or a file system
 * without ACLs — falls back to a fresh, randomly named temp directory.
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

    /** Loading seam: {@code loadBundled} / {@code System::loadLibrary} in production, fakes in tests. */
    @FunctionalInterface
    interface NativeLibraryLoader {
        void load(String libraryName);
    }

    /**
     * Load {@code libraryName} for {@link VesperaBridge#init(String)}: the
     * bundled (JAR-embedded) copy first, the system library path when none is
     * bundled; a link failure is rewrapped with deployment guidance.
     */
    static void loadNativeLibrary(
            String libraryName,
            NativeLibraryLoader bundledLoader,
            NativeLibraryLoader systemLoader) {
        try {
            try {
                bundledLoader.load(libraryName);
            } catch (BundledNativeAbsent absent) {
                loadFromSystemPath(libraryName, absent, systemLoader);
            }
        } catch (UnsatisfiedLinkError failure) {
            throw withDeploymentGuidance(failure, libraryName);
        }
    }

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

    /** The bundled resource chosen by {@link #resolveBundledResource}, already opened. */
    record BundledResource(String path, InputStream stream) {}

    static void loadBundled(String libraryName) {
        String os = detectOs();
        String filename = mapLibraryName(os, libraryName);
        ClassLoader loader = VesperaBridge.class.getClassLoader();
        List<String> platforms = NativePlatformDetector.hostCandidatePlatforms(NativePlatformDetector.HOST);
        BundledResource found;
        try {
            found = resolveBundledResource(platforms, filename, loader::getResourceAsStream);
        } catch (BundledNativeAbsent absent) {
            throw new BundledNativeAbsent(absent.getMessage() + BundledNativeIndex.describe(
                    platforms.get(0), filename, BundledNativeIndex.scan(loader, filename)));
        }
        String resourcePath = found.path();
        InputStream[] first = {found.stream()};
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

    /**
     * The first {@code native/<platform>/<filename>} resource present among
     * {@code platforms} (most specific first, see
     * {@link NativePlatformDetector#candidatePlatforms}). On a musl JVM that
     * bundles only the plain {@code linux-<arch>} (glibc) library, that one is
     * returned with a WARN: it will most likely fail to load.
     *
     * @param open opens a classpath resource, {@code null} when absent
     * @throws BundledNativeAbsent when none is bundled (names every path tried)
     */
    static BundledResource resolveBundledResource(
            List<String> platforms, String filename, Function<String, InputStream> open) {
        for (int i = 0; i < platforms.size(); i++) {
            String path = "native/" + platforms.get(i) + "/" + filename;
            InputStream stream = open.apply(path);
            if (stream == null) {
                continue;
            }
            String preferred = platforms.get(0);
            if (i > 0 && preferred.endsWith(NativePlatformDetector.MUSL_SUFFIX)) {
                LOG.log(System.Logger.Level.WARNING,
                        "No musl native library bundled (native/" + preferred + "/" + filename
                                + "); loading the glibc build " + path + " on a musl libc JVM,"
                                + " which will most likely fail with UnsatisfiedLinkError. Bundle a"
                                + " *-linux-musl build under native/" + preferred + "/ (Gradle plugin:"
                                + " vespera.targets) or override the detection with -D"
                                + NativePlatformDetector.LIBC_PROPERTY + "=glibc.");
            }
            return new BundledResource(path, stream);
        }
        StringBuilder tried = new StringBuilder();
        for (String platform : platforms) {
            tried.append(tried.length() == 0 ? "" : ", ")
                    .append("native/").append(platform).append('/').append(filename);
        }
        throw new BundledNativeAbsent("Not found in JAR: " + tried);
    }

    /**
     * The system-library-path fallback after {@code absent}: when that fails
     * too, the error carries both causes — the bundled paths tried and the
     * platforms the classpath does bundle — not only "no X in
     * java.library.path".
     */
    static void loadFromSystemPath(
            String libraryName, BundledNativeAbsent absent, NativeLibraryLoader systemLoader) {
        try {
            systemLoader.load(libraryName);
        } catch (UnsatisfiedLinkError notOnLibraryPath) {
            UnsatisfiedLinkError combined = new UnsatisfiedLinkError("Cannot load native library '"
                    + libraryName + "': " + absent.getMessage() + " System library path fallback: "
                    + notOnLibraryPath.getMessage());
            combined.initCause(notOnLibraryPath);
            combined.addSuppressed(absent);
            throw combined;
        }
    }

    /**
     * Rewrap the JVM's "already loaded in another classloader" link error —
     * a second web-app deployment (or a hot redeploy) loading the same
     * native library from its own class loader — with actionable guidance;
     * any other error is returned unchanged.
     */
    static UnsatisfiedLinkError withDeploymentGuidance(UnsatisfiedLinkError failure, String libraryName) {
        String message = failure.getMessage();
        if (message == null || !message.contains("already loaded in another classloader")) {
            return failure;
        }
        UnsatisfiedLinkError guided = new UnsatisfiedLinkError(message
                + " — the vespera native library '" + libraryName + "' can be loaded by only ONE"
                + " class loader per JVM and cannot be unloaded. On a servlet container that"
                + " redeploys or hosts several web apps, put vespera-bridge and the native"
                + " library in the container's shared lib directory (e.g. Tomcat $CATALINA_BASE/lib)"
                + " instead of WEB-INF/lib, run one deployment per JVM, and restart the JVM"
                + " (not only the web app) to pick up a new native build.");
        guided.initCause(failure);
        return guided;
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
            return prepareAclDir(baseDir, hashDirName);
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
            Path fresh = freshTempDir(baseDir.resolve(hashDirName), notPrivate,
                    PosixFilePermissions.asFileAttribute(OWNER_ONLY_DIR));
            fresh.toFile().deleteOnExit();
            LOG.log(System.Logger.Level.WARNING, "vespera: native extraction directory "
                    + baseDir.resolve(hashDirName) + " is not private to this user ("
                    + notPrivate.getMessage() + "); extracting into " + fresh
                    + " for this JVM only (no reuse). Point -D" + NATIVE_DIR_PROPERTY
                    + " at a directory owned by this user with mode 0700.");
            return new ExtractionDir(fresh, false);
        }
    }

    /**
     * Non-POSIX (Windows) counterpart of the POSIX checks: the base and hash
     * directories must be real directories (no symbolic link / junction),
     * owned by the current user, and their ACL must not grant write-like
     * rights to anyone but the owner, {@code SYSTEM} or
     * {@code Administrators}.  Otherwise — e.g. a directory another user
     * created first under a shared {@code java.io.tmpdir} to plant a DLL — the
     * library goes into a fresh, randomly named temp directory for this JVM
     * only.
     */
    static ExtractionDir prepareAclDir(Path baseDir, String hashDirName) throws IOException {
        Path dir = baseDir.resolve(hashDirName);
        try {
            Files.createDirectories(baseDir);
            requireOwnedAclDirectory(baseDir);
            try {
                Files.createDirectory(dir);
            } catch (FileAlreadyExistsException exists) {
                // Verified below.
            }
            requireOwnedAclDirectory(dir);
            return new ExtractionDir(dir, true);
        } catch (IOException | SecurityException | UnsupportedOperationException notPrivate) {
            Path fresh = freshTempDir(dir, notPrivate);
            fresh.toFile().deleteOnExit();
            LOG.log(System.Logger.Level.WARNING, "vespera: native extraction directory " + dir
                    + " is not private to this user (" + notPrivate.getMessage()
                    + "); extracting into " + fresh + " for this JVM only (no reuse). Point -D"
                    + NATIVE_DIR_PROPERTY + " at a directory only this user can write to.");
            return new ExtractionDir(fresh, false);
        }
    }

    /** ACL rights that let a principal add, replace or re-permission files in a directory. */
    private static final Set<AclEntryPermission> WRITE_LIKE = EnumSet.of(
            AclEntryPermission.WRITE_DATA, AclEntryPermission.APPEND_DATA,
            AclEntryPermission.DELETE_CHILD, AclEntryPermission.DELETE,
            AclEntryPermission.WRITE_ACL, AclEntryPermission.WRITE_OWNER);

    static void requireOwnedAclDirectory(Path dir) throws IOException {
        BasicFileAttributes attrs =
                Files.readAttributes(dir, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
        if (attrs.isSymbolicLink() || attrs.isOther() || !attrs.isDirectory()) {
            throw new UnsafeDirectoryException(dir + " is a link/junction or not a directory");
        }
        UserPrincipal owner = Files.getOwner(dir, LinkOption.NOFOLLOW_LINKS);
        UserPrincipal me = currentUser(dir);
        if (!owner.equals(me)) {
            throw new UnsafeDirectoryException(
                    dir + " is owned by " + owner.getName() + ", not " + me.getName());
        }
        AclFileAttributeView acl =
                Files.getFileAttributeView(dir, AclFileAttributeView.class, LinkOption.NOFOLLOW_LINKS);
        if (acl == null) {
            throw new UnsafeDirectoryException(dir + " has no ACL to verify");
        }
        Set<UserPrincipal> trusted = trustedPrincipals(dir, owner, me);
        String problem = aclProblem(acl.getAcl(), p -> trusted.contains(p) || isWellKnownAdmin(p));
        if (problem != null) {
            throw new UnsafeDirectoryException(dir + ": " + problem);
        }
    }

    /**
     * Why {@code acl} lets an untrusted principal write into the directory,
     * or {@code null}.  Inherit-only entries (which only apply to new
     * children, e.g. {@code CREATOR OWNER}), {@code DENY} entries and
     * principals Windows cannot resolve to an account (reported as a raw
     * {@code S-1-...} SID: deleted accounts, sandbox / capability SIDs — no
     * user can log on as them) are ignored.
     */
    static String aclProblem(List<AclEntry> acl, Predicate<UserPrincipal> trusted) {
        for (AclEntry entry : acl) {
            if (entry.type() != AclEntryType.ALLOW
                    || entry.flags().contains(AclEntryFlag.INHERIT_ONLY)
                    || isUnresolvedSid(entry.principal())
                    || trusted.test(entry.principal())) {
                continue;
            }
            for (AclEntryPermission permission : entry.permissions()) {
                if (WRITE_LIKE.contains(permission)) {
                    return entry.principal().getName() + " is granted " + permission;
                }
            }
        }
        return null;
    }

    private static Set<UserPrincipal> trustedPrincipals(Path dir, UserPrincipal owner, UserPrincipal me) {
        Set<UserPrincipal> trusted = new HashSet<>();
        trusted.add(owner);
        trusted.add(me);
        for (String name : WELL_KNOWN_ADMINS) {
            try {
                trusted.add(dir.getFileSystem().getUserPrincipalLookupService().lookupPrincipalByName(name));
            } catch (IOException | UnsupportedOperationException unknown) {
                // Localised or unavailable: isWellKnownAdmin's name check still applies.
            }
        }
        return trusted;
    }

    private static final String[] WELL_KNOWN_ADMINS = {"NT AUTHORITY\\SYSTEM", "BUILTIN\\Administrators"};

    static boolean isUnresolvedSid(UserPrincipal principal) {
        return principal.getName().startsWith("S-1-");
    }

    /** {@code SYSTEM} / {@code Administrators} by (English) account name. */
    static boolean isWellKnownAdmin(UserPrincipal principal) {
        String name = principal.getName();
        for (String admin : WELL_KNOWN_ADMINS) {
            if (admin.equalsIgnoreCase(name)) {
                return true;
            }
        }
        return false;
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
        UserPrincipal me = currentUser(dir);
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
     * The current OS user: the owner of a freshly created probe file (robust
     * where {@code user.name} has no passwd entry, e.g. containers). The
     * probe is created in {@code probeDir} — the extraction directory being
     * verified — and only falls back to {@code java.io.tmpdir} when that
     * fails, so {@code vespera.native.dir} works on hosts whose system temp
     * directory is missing or read-only.
     */
    static UserPrincipal currentUser(Path probeDir) throws IOException {
        UserPrincipal cached = cachedCurrentUser;
        if (cached != null) {
            return cached;
        }
        Path probe;
        try {
            probe = Files.createTempFile(probeDir, "vespera-owner-", ".probe");
        } catch (IOException | SecurityException | UnsupportedOperationException inDir) {
            try {
                probe = Files.createTempFile("vespera-owner-", ".probe");
            } catch (IOException inTmp) {
                inTmp.addSuppressed(inDir);
                throw inTmp;
            }
        }
        try {
            cached = Files.getOwner(probe, LinkOption.NOFOLLOW_LINKS);
        } finally {
            Files.deleteIfExists(probe);
        }
        cachedCurrentUser = cached;
        return cached;
    }

    /**
     * A fresh, randomly named directory in {@code java.io.tmpdir} for this
     * JVM only (the fallback when {@code notPrivate} rejected the configured
     * one). When the system temp directory is unusable too, the failure
     * names both problems instead of only the temp-directory one.
     */
    private static Path freshTempDir(Path rejected, Exception notPrivate,
            java.nio.file.attribute.FileAttribute<?>... attrs) throws IOException {
        try {
            return Files.createTempDirectory("vespera-native-", attrs);
        } catch (IOException tmpUnusable) {
            IOException failure = new IOException("native extraction directory " + rejected
                    + " is not usable (" + notPrivate.getMessage() + ") and no fallback directory could be"
                    + " created in java.io.tmpdir (" + tmpUnusable + "). Point -D" + NATIVE_DIR_PROPERTY
                    + " at a writable directory owned by this user.", tmpUnusable);
            failure.addSuppressed(notPrivate);
            throw failure;
        }
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
        return NativePlatformDetector.detectOs(System.getProperty("os.name", ""));
    }

    private static String detectArch() {
        return NativePlatformDetector.detectArch(System.getProperty("os.arch", ""));
    }

    private static String mapLibraryName(String os, String name) {
        return switch (os) {
            case "windows" -> name + ".dll";
            case "macos" -> "lib" + name + ".dylib";
            default -> "lib" + name + ".so";
        };
    }
}
