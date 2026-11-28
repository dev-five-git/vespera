package com.devfive.vespera.bridge;

import java.io.IOException;
import java.nio.file.DirectoryStream;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.SecureDirectoryStream;
import java.nio.file.attribute.BasicFileAttributes;
import java.nio.file.attribute.PosixFileAttributeView;
import java.nio.file.attribute.PosixFileAttributes;
import java.nio.file.attribute.UserPrincipal;
import java.util.ArrayList;
import java.util.List;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Best-effort removal of the fresh per-JVM extraction directories a previous
 * run left in the system temp directory (see {@link VesperaNativeLoader}).
 *
 * <p>The temp directory is shared, so a directory another user controls can
 * carry a fresh-looking name, and an entry of ours can be replaced by a link
 * between a check and the next operation on it. Therefore:
 * <ul>
 *   <li>a name is a candidate only when it cannot be a content-addressed cache
 *       directory: {@link #FRESH_PREFIX} followed by digits, or the legacy
 *       digits-only {@code Files.createTempDirectory("vespera-native-")} name
 *       whose length is not that of a cache directory's hash;</li>
 *   <li>a directory is removed only when it is a real directory (no link, no
 *       other file type), owned by the current user, and older than
 *       {@link VesperaNativeLoader#LEGACY_MIN_AGE_MILLIS};</li>
 *   <li>where the file system supports {@link SecureDirectoryStream} (POSIX)
 *       every step is relative to an open file descriptor, with
 *       {@code NOFOLLOW_LINKS}: a link is never followed, and the directory
 *       opened is verified to be the one checked (same file key, owner);
 *       only regular files owned by the current user are unlinked, then the
 *       directory itself (which fails unless it is empty);</li>
 *   <li>elsewhere (Windows) the same checks apply by path, without a
 *       secure stream: the directory and each entry are read with
 *       {@code NOFOLLOW_LINKS} and must be owned by the current user; a
 *       directory still holding a loaded library is left (its file is locked
 *       until its JVM exits).</li>
 * </ul>
 * Nothing outside a directory matching these rules is ever deleted.
 */
final class NativeExtractionSweep {

    /** Name prefix of a fresh per-JVM directory ({@code Files.createTempDirectory}). */
    static final String FRESH_PREFIX = "vespera-native-fresh-";

    /** {@code vespera-native-<digits>} (legacy) or {@code vespera-native-fresh-<digits>}. */
    private static final Pattern FRESH_NAME = Pattern.compile("vespera-native-(fresh-)?(\\d+)");

    private NativeExtractionSweep() {
    }

    /**
     * Whether {@code name} may be a fresh per-JVM directory and never a
     * content-addressed cache directory ({@code vespera-native-} followed by
     * {@link VesperaNativeLoader#DIGEST_PREFIX_HEX} hex digits): a legacy
     * digits-only name of exactly that length could be one, so it is kept.
     */
    static boolean isFreshDirName(String name) {
        Matcher matcher = FRESH_NAME.matcher(name);
        if (!matcher.matches()) {
            return false;
        }
        return matcher.group(1) != null || matcher.group(2).length() != VesperaNativeLoader.DIGEST_PREFIX_HEX;
    }

    /**
     * Sweep {@code tmpDir} as the current user. {@code -Dvespera.native.cleanupLegacy=false}
     * disables it. Returns the number of directories removed.
     */
    static int sweep(Path tmpDir, long nowMillis) {
        if ("false".equalsIgnoreCase(System.getProperty("vespera.native.cleanupLegacy"))) {
            return 0;
        }
        try {
            return sweep(tmpDir, nowMillis, VesperaNativeLoader.currentUser());
        } catch (IOException | SecurityException noCurrentUser) {
            return 0;
        }
    }

    /** {@link #sweep(Path, long)} for the given owner: directories and files must belong to {@code me}. */
    static int sweep(Path tmpDir, long nowMillis, UserPrincipal me) {
        try (DirectoryStream<Path> listing = Files.newDirectoryStream(tmpDir)) {
            List<Path> names = namesIn(listing);
            int removed = 0;
            for (Path name : names) {
                if (!isFreshDirName(name.toString())) {
                    continue;
                }
                try {
                    boolean done = listing instanceof SecureDirectoryStream<Path> root
                            ? removeSecure(root, name, me, nowMillis)
                            : removeByPath(tmpDir.resolve(name), me, nowMillis);
                    if (done) {
                        removed++;
                    }
                } catch (IOException | SecurityException inUseOrNotOurs) {
                    // Still loaded (Windows), not ours, a link, or not empty: leave it.
                }
            }
            return removed;
        } catch (IOException | SecurityException unreadableDir) {
            // Cleanup is purely opportunistic.
            return 0;
        }
    }

    /**
     * POSIX: {@code name} is a directory of {@code root} opened without
     * following links, and every operation on it is relative to that
     * descriptor.
     */
    static boolean removeSecure(SecureDirectoryStream<Path> root, Path name, UserPrincipal me, long nowMillis)
            throws IOException {
        PosixFileAttributes checked = root.getFileAttributeView(name, PosixFileAttributeView.class,
                LinkOption.NOFOLLOW_LINKS).readAttributes();
        if (!removable(checked, me, nowMillis)) {
            return false;
        }
        try (SecureDirectoryStream<Path> dir = root.newDirectoryStream(name, LinkOption.NOFOLLOW_LINKS)) {
            PosixFileAttributes opened = dir.getFileAttributeView(PosixFileAttributeView.class).readAttributes();
            if (!checked.fileKey().equals(opened.fileKey()) || !removable(opened, me, nowMillis)) {
                return false;
            }
            for (Path entry : namesIn(dir)) {
                PosixFileAttributes attrs = dir.getFileAttributeView(entry, PosixFileAttributeView.class,
                        LinkOption.NOFOLLOW_LINKS).readAttributes();
                if (attrs.isRegularFile() && me.equals(attrs.owner())) {
                    try {
                        dir.deleteFile(entry);
                    } catch (IOException loaded) {
                        // Not deletable right now: the directory stays.
                    }
                }
            }
        }
        // Still the directory that was checked? Then remove it (fails unless empty).
        if (!checked.fileKey().equals(root.getFileAttributeView(name, PosixFileAttributeView.class,
                LinkOption.NOFOLLOW_LINKS).readAttributes().fileKey())) {
            return false;
        }
        root.deleteDirectory(name);
        return true;
    }

    /** Non-POSIX: the same checks by path, with {@code NOFOLLOW_LINKS} on the directory and each entry. */
    static boolean removeByPath(Path dir, UserPrincipal me, long nowMillis) throws IOException {
        BasicFileAttributes attrs = Files.readAttributes(dir, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
        if (attrs.isSymbolicLink() || attrs.isOther() || !attrs.isDirectory()
                || !me.equals(Files.getOwner(dir, LinkOption.NOFOLLOW_LINKS))
                || nowMillis - attrs.lastModifiedTime().toMillis() < VesperaNativeLoader.LEGACY_MIN_AGE_MILLIS) {
            return false;
        }
        try (DirectoryStream<Path> entries = Files.newDirectoryStream(dir)) {
            for (Path entry : namesIn(entries)) {
                Path file = dir.resolve(entry);
                BasicFileAttributes fileAttrs =
                        Files.readAttributes(file, BasicFileAttributes.class, LinkOption.NOFOLLOW_LINKS);
                if (fileAttrs.isRegularFile() && me.equals(Files.getOwner(file, LinkOption.NOFOLLOW_LINKS))) {
                    try {
                        Files.deleteIfExists(file);
                    } catch (IOException | SecurityException loaded) {
                        // Locked by a live JVM (Windows): leave it.
                    }
                }
            }
        }
        Files.deleteIfExists(dir);
        return true;
    }

    /** A real directory owned by {@code me} and older than the sweep's minimum age. */
    private static boolean removable(PosixFileAttributes attrs, UserPrincipal me, long nowMillis) {
        return attrs.isDirectory() && !attrs.isSymbolicLink() && me.equals(attrs.owner())
                && nowMillis - attrs.lastModifiedTime().toMillis() >= VesperaNativeLoader.LEGACY_MIN_AGE_MILLIS;
    }

    private static List<Path> namesIn(DirectoryStream<Path> stream) throws IOException {
        List<Path> names = new ArrayList<>();
        for (Path path : stream) {
            names.add(path.getFileName());
        }
        return names;
    }
}
