package com.devfive.vespera.bridge;

import java.io.IOException;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.attribute.BasicFileAttributes;
import java.nio.file.attribute.PosixFileAttributes;
import java.nio.file.attribute.PosixFilePermission;
import java.nio.file.attribute.UserPrincipal;
import java.nio.file.attribute.UserPrincipalNotFoundException;
import java.util.List;
import java.util.Locale;
import java.util.Objects;

/**
 * "The current OS user" for the ownership checks of
 * {@link VesperaNativeLoader} — worked out <em>without</em> trusting the
 * directory those checks are still verifying.
 *
 * <p>The answer decides whether an extraction directory (and the library a
 * later {@code System.load} runs) belongs to this user, so a source another
 * local user can influence must never produce it: a probe file created in an
 * attacker-owned directory can be swapped for one the attacker owns between
 * its creation and {@code Files.getOwner}. The sources are tried in order:
 * <ol>
 *   <li>Linux: the owner of {@code /proc/self} on a {@code proc} file system
 *       — the kernel's answer (the process's effective uid), no file
 *       created;</li>
 *   <li>the owner of a probe file created in {@code java.io.tmpdir} (sticky
 *       {@code /tmp}, or a per-user temp directory on macOS / Windows: no
 *       other user can replace it);</li>
 *   <li>{@code user.name} looked up by the file system's principal lookup
 *       service;</li>
 *   <li>last resort, only when all of the above are unavailable (no procfs,
 *       no usable {@code java.io.tmpdir} — read-only root file system
 *       containers — and no resolvable {@code user.name}): a probe file in
 *       the directory being verified, and only while that directory is a
 *       real directory that is not group/other-writable, before and after the
 *       probe, with the same file key.</li>
 * </ol>
 * Answers from sources 1–3 are cached for the JVM; the last-resort answer is
 * never cached, so one unverified directory cannot decide the identity used
 * for every later check.
 */
final class CurrentUser {

    /** The individual sources; replaceable by tests. */
    interface Sources {
        /** Kernel-reported owner of this process, or {@code null} where unavailable. */
        UserPrincipal processOwner() throws IOException;

        /** Owner of a probe file in {@code java.io.tmpdir}. */
        UserPrincipal tmpdirProbeOwner() throws IOException;

        /** {@code user.name} resolved to a principal, or {@code null}. */
        UserPrincipal userNameLookup() throws IOException;

        /** Owner of a probe file created in {@code dir}. */
        UserPrincipal probeOwnerIn(Path dir) throws IOException;
    }

    static final Sources SYSTEM = new Sources() {
        @Override
        public UserPrincipal processOwner() throws IOException {
            String os = System.getProperty("os.name", "");
            if (!os.toLowerCase(Locale.ROOT).startsWith("linux")) {
                return null;
            }
            Path proc = Paths.get("/proc");
            if (!Files.isDirectory(proc, LinkOption.NOFOLLOW_LINKS)
                    || !"proc".equals(Files.getFileStore(proc).type())) {
                return null;
            }
            // Follows the /proc/self link to /proc/<pid>, owned by the euid.
            return Files.getOwner(proc.resolve("self"));
        }

        @Override
        public UserPrincipal tmpdirProbeOwner() throws IOException {
            Path tmp = Paths.get(System.getProperty("java.io.tmpdir", "/tmp"));
            UserPrincipal self = processOwner();
            if (self == null) {
                self = userNameLookup();
            }
            if (!trustedTmpdir(tmp, self)) {
                // Anyone could swap the probe in a directory another user controls: let
                // the principal lookup answer instead.
                return null;
            }
            return ownerOfProbe(Files.createTempFile(tmp, "vespera-owner-", ".probe"));
        }

        @Override
        public UserPrincipal userNameLookup() throws IOException {
            String name = System.getProperty("user.name");
            if (name == null || name.isBlank() || "?".equals(name)) {
                return null;
            }
            try {
                return FileSystems.getDefault().getUserPrincipalLookupService()
                        .lookupPrincipalByName(name);
            } catch (UserPrincipalNotFoundException unknown) {
                return null;
            }
        }

        @Override
        public UserPrincipal probeOwnerIn(Path dir) throws IOException {
            return ownerOfProbe(Files.createTempFile(dir, "vespera-owner-", ".probe"));
        }
    };

    static volatile Sources sources = SYSTEM;

    private static volatile UserPrincipal cached;

    private CurrentUser() {}

    /** Forget the cached answer (tests). */
    static void resetCache() {
        cached = null;
    }

    /** The current user; {@code dirBeingVerified} is consulted only as the guarded last resort. */
    static UserPrincipal of(Path dirBeingVerified) throws IOException {
        UserPrincipal known = cached;
        if (known != null) {
            return known;
        }
        Sources s = sources;
        IOException failures = new IOException("cannot determine the current user");
        UserPrincipal trusted = firstTrusted(s, failures);
        if (trusted != null) {
            cached = trusted;
            return trusted;
        }
        try {
            return guardedProbe(s, dirBeingVerified);
        } catch (IOException | SecurityException | UnsupportedOperationException inDir) {
            failures.addSuppressed(inDir);
            throw failures;
        }
    }

    private static UserPrincipal firstTrusted(Sources s, IOException failures) {
        try {
            UserPrincipal owner = s.processOwner();
            if (owner != null) {
                return owner;
            }
        } catch (IOException | SecurityException | UnsupportedOperationException e) {
            failures.addSuppressed(e);
        }
        try {
            // null = java.io.tmpdir is not trusted: the next source answers, the
            // directory being verified never does.
            UserPrincipal owner = s.tmpdirProbeOwner();
            if (owner != null) {
                return owner;
            }
        } catch (IOException | SecurityException | UnsupportedOperationException e) {
            failures.addSuppressed(e);
        }
        try {
            return s.userNameLookup();
        } catch (IOException | SecurityException | UnsupportedOperationException e) {
            failures.addSuppressed(e);
        }
        return null;
    }

    private static UserPrincipal guardedProbe(Sources s, Path dir) throws IOException {
        Object before = requireProbeable(dir);
        UserPrincipal owner = s.probeOwnerIn(dir);
        if (!Objects.equals(before, requireProbeable(dir))) {
            throw new VesperaNativeLoader.UnsafeDirectoryException(
                    dir + " changed while probing the current user");
        }
        return owner;
    }

    /**
     * A real directory nobody but its owner may write to (POSIX), else
     * {@link VesperaNativeLoader.UnsafeDirectoryException}; returns its file
     * key (or path) and, on POSIX, its owner, so a swapped or re-owned
     * directory is detected.
     */
    private static Object requireProbeable(Path dir) throws IOException {
        if (VesperaNativeLoader.posixSupported(dir)) {
            PosixFileAttributes attrs =
                    Files.readAttributes(dir, PosixFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
            if (attrs.isSymbolicLink() || !attrs.isDirectory()
                    || attrs.permissions().contains(PosixFilePermission.GROUP_WRITE)
                    || attrs.permissions().contains(PosixFilePermission.OTHERS_WRITE)) {
                throw new VesperaNativeLoader.UnsafeDirectoryException(dir
                        + " is a link, not a directory or group/other-writable: not probed");
            }
            return List.of(key(attrs, dir), attrs.owner());
        }
        BasicFileAttributes attrs =
                Files.readAttributes(dir, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
        if (attrs.isSymbolicLink() || attrs.isOther() || !attrs.isDirectory()) {
            throw new VesperaNativeLoader.UnsafeDirectoryException(
                    dir + " is a link/junction or not a directory: not probed");
        }
        return key(attrs, dir);
    }

    private static Object key(BasicFileAttributes attrs, Path dir) {
        Object key = attrs.fileKey();
        return key != null ? key : dir.toAbsolutePath();
    }

    /**
     * Whether {@code java.io.tmpdir} may host the probe. On POSIX it must be a
     * directory that is either root-owned and sticky ({@code /tmp}: no other
     * user can replace or delete a file it does not own), or owned by
     * {@code self} (the process's identity, {@code null} when unknown: then
     * untrusted) and not group/other-writable unless sticky. A directory
     * another user owns is never trusted, even when it is 0755: its owner can
     * rename the probe before it is read. Non-POSIX file systems keep the
     * per-user temp directory default.
     */
    static boolean trustedTmpdir(Path dir, UserPrincipal self) throws IOException {
        if (!VesperaNativeLoader.posixSupported(dir)) {
            return true;
        }
        PosixFileAttributes attrs = Files.readAttributes(dir, PosixFileAttributes.class);
        if (!attrs.isDirectory()) {
            return false;
        }
        boolean sticky = sticky(dir);
        if (sticky && "root".equals(attrs.owner().getName())) {
            return true;
        }
        boolean shared = attrs.permissions().contains(PosixFilePermission.GROUP_WRITE)
                || attrs.permissions().contains(PosixFilePermission.OTHERS_WRITE);
        return self != null && self.equals(attrs.owner()) && (!shared || sticky);
    }

    /** The sticky bit, read through the {@code unix} view (absent on some file systems: not sticky). */
    private static boolean sticky(Path dir) throws IOException {
        try {
            return Files.getAttribute(dir, "unix:mode") instanceof Integer bits
                    && (bits & 01000) != 0;
        } catch (UnsupportedOperationException | IllegalArgumentException noUnixView) {
            return false;
        }
    }

    private static UserPrincipal ownerOfProbe(Path probe) throws IOException {
        try {
            return Files.getOwner(probe, LinkOption.NOFOLLOW_LINKS);
        } finally {
            Files.deleteIfExists(probe);
        }
    }
}
