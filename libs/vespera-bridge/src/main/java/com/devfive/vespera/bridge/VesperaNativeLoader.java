package com.devfive.vespera.bridge;

import java.io.IOException;
import java.io.InputStream;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.DirectoryStream;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.InvalidPathException;
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
 * before a cached copy is trusted, and every ancestor of the base must be
 * owned by this user or {@code root} and not group/other-writable unless
 * sticky (see {@link #requireSafeAncestors}); the base is resolved to its
 * real path once and the library is extracted, verified and loaded through
 * that path, so the checked chain is the loaded chain and another local user can
 * neither plant nor swap the library between the digest check and
 * {@code System.load}.
 * "The current user" never comes from the directory being verified while
 * a trusted source answers ({@link CurrentUser}).
 * The extracted file is made owner read/execute only ({@code 0500}).  When
 * verification fails (a squatted, shared or symlinked directory) the
 * library is extracted into a fresh private
 * {@code Files.createTempDirectory("vespera-native-")} for this JVM only,
 * with no reuse, and a warning is logged.  On file systems without POSIX
 * attributes (Windows) the base and the hash directory are checked for their
 * owner and ACL (see {@link #prepareAclDir}); a directory that fails it — or a
 * file system without ACLs — falls back to a fresh, randomly named temp
 * directory. Unlike the POSIX path, the ancestors of the base directory are
 * not checked there: a user who may rename a shared base directory's parent
 * can still swap the directory between the check and {@code System.load}.
 * Point {@code -Dvespera.native.dir} only at a location in a
 * directory this user alone can modify (the default temp location is).
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
    /**
     * A per-JVM fresh directory ({@code Files.createTempDirectory("vespera-native-")}
     * in {@code java.io.tmpdir}, used when the configured directory is not private).
     */
    private static final Pattern FRESH_EXTRACTION_DIR = Pattern.compile("vespera-native-\\d+");
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
        Extracted extracted;
        try {
            // Inside the try: an invalid directory must still close the
            // bundled resource stream opened above.
            Path baseDir = resolveExtractionBaseDir(System::getProperty, System::getenv);
            try {
                extracted = extractCopy(resourcePath, filename, baseDir, opener);
            } catch (IOException e) {
                UnsatisfiedLinkError ule = new UnsatisfiedLinkError("Extract failed: " + e.getMessage()
                        + " (extraction dir " + baseDir + "; override with -D" + NATIVE_DIR_PROPERTY
                        + " or " + NATIVE_DIR_ENV + ")");
                ule.initCause(e);
                throw ule;
            }
        } finally {
            closeQuietly(first[0]);
        }
        loadExtracted(extracted, System::load, Files::deleteIfExists);
        try {
            Path tmp = Paths.get(System.getProperty("java.io.tmpdir"));
            long now = System.currentTimeMillis();
            cleanupLegacyExtractions(tmp, now);
            cleanupFreshExtractions(tmp, now);
        } catch (InvalidPathException ignored) {
            // Best-effort cleanup; the library is already loaded.
        }
    }

    /**
     * {@link #resolveBaseDir}, with a path the file system rejects (literal
     * quotes from a Windows shell: {@code -Dvespera.native.dir="C:\native"})
     * turned into the usual {@link UnsatisfiedLinkError} naming the settings
     * to fix instead of a raw {@link InvalidPathException}.
     */
    static Path resolveExtractionBaseDir(UnaryOperator<String> properties, UnaryOperator<String> env) {
        try {
            return resolveBaseDir(properties, env);
        } catch (InvalidPathException invalid) {
            UnsatisfiedLinkError ule = new UnsatisfiedLinkError("Invalid native extraction directory: "
                    + invalid.getMessage() + " (set -D" + NATIVE_DIR_PROPERTY + " or " + NATIVE_DIR_ENV
                    + " to a valid directory path, or fix java.io.tmpdir)");
            ule.initCause(invalid);
            throw ule;
        }
    }

    /**
     * {@code System.load} the verified extracted copy. A private copy that
     * cannot be loaded here is deleted with its temp directory: its bytes match
     * the bundled resource, so it is dead weight. A copy in the shared
     * content-addressed directory is never deleted, whatever the failure: a
     * concurrent JVM may have verified the same file and be loading it now, and
     * would then fail with "cannot open shared object" instead of its own load
     * error. The shared copy is re-verified (and re-extracted if corrupt) on the
     * next start. A load refused only because another class loader already holds
     * the library ("already loaded in another classloader") keeps its copy too.
     */
    static void loadExtracted(Extracted copy, NativeLibraryLoader systemLoad, TempFileDeleter deleter) {
        Path library = copy.library();
        try {
            systemLoad.load(library.toAbsolutePath().toString());
        } catch (UnsatisfiedLinkError loadFailure) {
            if (!copy.reusable() && !loadedByAnotherClassLoader(loadFailure)) {
                // Only this JVM's private temp directory is removed with the copy.
                deleteAfterFailedLoad(library, deleter);
                deleteAfterFailedLoad(library.getParent(), deleter);
            }
            throw loadFailure;
        }
    }

    /** An extracted library and whether its directory is the shared, reusable one. */
    record Extracted(Path library, boolean reusable) {}

    /** The JVM's "Native Library ... already loaded in another classloader" failure. */
    static boolean loadedByAnotherClassLoader(UnsatisfiedLinkError failure) {
        String message = failure.getMessage();
        return message != null && message.contains("already loaded in another classloader");
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
        if (!loadedByAnotherClassLoader(failure)) {
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
            // Resolve once and use the real path from here on (hash dir,
            // digest check, System.load): the ancestors checked are then the
            // ones the library is loaded through. A symbolic link in the
            // middle of the configured path (`/shared/app` -> a private
            // tree) would otherwise be checked as its target while the load
            // still went by name through a group-writable `/shared`, where
            // another user could swap the link after the digest check.
            Path realBase = baseDir.toRealPath();
            requireSafeAncestors(realBase);
            Path dir = realBase.resolve(hashDirName);
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
     * {@code Administrators} (see {@link #aclProblem} for the unresolved-SID
     * rule: an inherited stale grant does not count).  Otherwise — e.g. a directory another user
     * created first under a shared {@code java.io.tmpdir} to plant a DLL — the
     * library goes into a fresh, randomly named temp directory for this JVM
     * only.
     */
    static ExtractionDir prepareAclDir(Path baseDir, String hashDirName) throws IOException {
        Path dir = baseDir.resolve(hashDirName);
        try {
            Files.createDirectories(baseDir);
            requireOwnedAclDirectory(baseDir);
            // As on POSIX: extract and load through the path that was checked.
            dir = baseDir.toRealPath().resolve(hashDirName);
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
        UserPrincipal me = currentUser();
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
        String problem = aclProblem(acl.getAcl(), p -> trusted.contains(p) || isWellKnownAdmin(p),
                parentAcl(dir));
        if (problem != null) {
            throw new UnsafeDirectoryException(dir + ": " + problem);
        }
    }

    /**
     * The ACL of {@code dir}'s parent, against which {@link #aclProblem} tells
     * an inherited entry from an explicit one. An unreadable parent yields no
     * entries, so every entry then counts as explicit (the strict reading).
     */
    private static List<AclEntry> parentAcl(Path dir) {
        Path parent = dir.toAbsolutePath().getParent();
        if (parent == null) {
            return List.of();
        }
        try {
            AclFileAttributeView view = Files.getFileAttributeView(parent, AclFileAttributeView.class);
            return view == null ? List.of() : view.getAcl();
        } catch (IOException | SecurityException unreadable) {
            return List.of();
        }
    }

    /**
     * Why {@code acl} lets an untrusted principal write into the directory,
     * or {@code null}.  Inherit-only entries (which only apply to new
     * children, e.g. {@code CREATOR OWNER}), {@code DENY} entries and
     * app-container / capability SIDs ({@link #isCapabilitySid}, {@code S-1-15-...},
     * no user can log on as them) are ignored.
     *
     * <p>A principal Windows cannot resolve to a name (a raw {@code S-1-...}
     * SID, e.g. a domain account that is unreachable right now) is untrusted
     * like a named one when its write-like rights are explicit on this
     * directory. When the same grant is inherited from {@code parentAcl} (a
     * stale SID on {@code %TEMP%} that every new child directory picks up) it is
     * not: that is the normal state of many domain-joined machines, and
     * refusing it would fall back to a fresh copy on every start.
     */
    static String aclProblem(List<AclEntry> acl, Predicate<UserPrincipal> trusted,
            List<AclEntry> parentAcl) {
        for (AclEntry entry : acl) {
            if (entry.type() != AclEntryType.ALLOW
                    || entry.flags().contains(AclEntryFlag.INHERIT_ONLY)
                    || isCapabilitySid(entry.principal())
                    || trusted.test(entry.principal())
                    || (isUnresolvedSid(entry.principal()) && inheritedFrom(entry, parentAcl))) {
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

    /** A principal Windows lists by raw SID because it resolves to no name. */
    static boolean isUnresolvedSid(UserPrincipal principal) {
        return principal.getName().startsWith("S-1-");
    }

    /**
     * Whether {@code entry} is already granted by {@code parentAcl}: the same
     * principal and type with at least its permissions. Windows copies an
     * inherited entry into the child unchanged apart from its inheritance flags,
     * and the Java API does not expose the inherited bit, so this comparison is
     * how an inherited entry is recognised.
     */
    static boolean inheritedFrom(AclEntry entry, List<AclEntry> parentAcl) {
        for (AclEntry parent : parentAcl) {
            if (parent.type() == entry.type()
                    && parent.principal().getName().equals(entry.principal().getName())
                    && parent.permissions().containsAll(entry.permissions())) {
                return true;
            }
        }
        return false;
    }

    /**
     * {@code S-1-15-*}: app-container and capability SIDs (for example
     * {@code ALL APPLICATION PACKAGES}), which Windows lists by raw SID. They
     * are not accounts, so nothing can log on as them.
     */
    static boolean isCapabilitySid(UserPrincipal principal) {
        return principal.getName().startsWith("S-1-15-");
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
        // Mode first: a group/other-writable directory is rejected before
        // anything else looks at it.
        Set<PosixFilePermission> perms = attrs.permissions();
        if (perms.contains(PosixFilePermission.GROUP_WRITE)
                || perms.contains(PosixFilePermission.OTHERS_WRITE)) {
            throw new UnsafeDirectoryException(dir + " is group/other-writable ("
                    + PosixFilePermissions.toString(perms) + ")");
        }
        UserPrincipal me = currentUser();
        if (!attrs.owner().equals(me)) {
            throw new UnsafeDirectoryException(
                    dir + " is owned by " + attrs.owner().getName() + ", not " + me.getName());
        }
    }

    /**
     * POSIX: no other user can swap a component of the path to
     * {@code baseDir} (a private directory inside a group-writable
     * {@code /shared/apps} could be renamed away and replaced, and
     * {@code System.load} resolves the path by name). Every ancestor of its
     * real path is owned by this user or {@code root} and is not
     * group/other-writable unless it has the sticky bit ({@code /tmp}, where
     * only an entry's owner may rename it — the next component is checked
     * for its owner in turn).
     */
    static void requireSafeAncestors(Path baseDir) throws IOException {
        UserPrincipal me = currentUser();
        for (Path dir = baseDir.toRealPath().getParent(); dir != null; dir = dir.getParent()) {
            PosixFileAttributes attrs =
                    Files.readAttributes(dir, PosixFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
            String owner = attrs.owner().getName();
            if (!attrs.owner().equals(me) && !"root".equals(owner)) {
                throw new UnsafeDirectoryException(
                        dir + " (an ancestor of " + baseDir + ") is owned by " + owner);
            }
            Set<PosixFilePermission> perms = attrs.permissions();
            if ((perms.contains(PosixFilePermission.GROUP_WRITE)
                    || perms.contains(PosixFilePermission.OTHERS_WRITE)) && !sticky(dir)) {
                throw new UnsafeDirectoryException(dir + " (an ancestor of " + baseDir
                        + ") is group/other-writable without the sticky bit ("
                        + PosixFilePermissions.toString(perms) + ")");
            }
        }
    }

    /** Whether {@code dir} has the sticky bit ({@code false} when the file system cannot tell). */
    static boolean sticky(Path dir) {
        try {
            return Files.getAttribute(dir, "unix:mode", LinkOption.NOFOLLOW_LINKS) instanceof Integer mode
                    && (mode & 01000) != 0;
        } catch (IOException | UnsupportedOperationException | IllegalArgumentException unknown) {
            return false;
        }
    }

    /** The current OS user from a trusted source only; fails closed (see {@link CurrentUser}). */
    static UserPrincipal currentUser() throws IOException {
        return CurrentUser.of();
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
        return extractCopy(resourcePath, filename, baseDir, opener).library();
    }

    /** {@link #extract}, also telling whether the copy lives in the reusable directory. */
    static Extracted extractCopy(String resourcePath, String filename, Path baseDir, ResourceOpener opener)
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
            return new Extracted(target, true);
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
            return new Extracted(target, prepared.reusable());
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

    /**
     * Best-effort removal of the fresh per-JVM directories a previous run left
     * in {@code tmpDir} ({@link #FRESH_EXTRACTION_DIR}, older than
     * {@link #LEGACY_MIN_AGE_MILLIS}). Each file is deleted on its own, then the
     * directory if it is empty: on Windows the copy a live JVM has loaded is
     * locked, so its delete fails and that directory stays until its JVM exits,
     * which is the only time it can go. Returns the number of directories removed.
     */
    static int cleanupFreshExtractions(Path tmpDir, long nowMillis) {
        if ("false".equalsIgnoreCase(System.getProperty("vespera.native.cleanupLegacy"))) {
            return 0;
        }
        int removed = 0;
        try (DirectoryStream<Path> dirs = Files.newDirectoryStream(tmpDir, "vespera-native-*")) {
            for (Path dir : dirs) {
                try {
                    if (!FRESH_EXTRACTION_DIR.matcher(dir.getFileName().toString()).matches()
                            || !Files.isDirectory(dir, LinkOption.NOFOLLOW_LINKS)
                            || nowMillis - Files.getLastModifiedTime(dir).toMillis() < LEGACY_MIN_AGE_MILLIS) {
                        continue;
                    }
                    try (DirectoryStream<Path> files = Files.newDirectoryStream(dir)) {
                        for (Path file : files) {
                            try {
                                Files.deleteIfExists(file);
                            } catch (IOException | SecurityException loaded) {
                                // Locked by a live JVM (Windows) or not ours: leave it.
                            }
                        }
                    }
                    Files.deleteIfExists(dir);
                    removed++;
                } catch (IOException | SecurityException inUseOrDenied) {
                    // Not empty (a file is still loaded) or not ours: leave it.
                }
            }
        } catch (IOException | SecurityException unreadableDir) {
            // Cleanup is purely opportunistic.
        }
        return removed;
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
