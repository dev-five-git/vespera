package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.attribute.FileTime;
import java.nio.file.attribute.UserPrincipal;
import java.util.Comparator;
import org.junit.jupiter.api.Test;

/** {@link NativeExtractionSweep}: which fresh directories are removed, and never anything else. */
class NativeExtractionSweepTest {

    private static final long NOW = System.currentTimeMillis();
    private static final FileTime OLD =
            FileTime.fromMillis(NOW - VesperaNativeLoader.LEGACY_MIN_AGE_MILLIS - 60_000);

    private static boolean posix() {
        return Path.of(System.getProperty("java.io.tmpdir")).getFileSystem()
                .supportedFileAttributeViews().contains("posix");
    }

    /** A principal that owns nothing: every candidate is "not ours". */
    private static final UserPrincipal SOMEONE_ELSE = new UserPrincipal() {
        @Override
        public String getName() {
            return "someone-else";
        }
    };

    @Test
    void onlyFreshOrNonCacheNamesAreCandidates() {
        assertTrue(NativeExtractionSweep.isFreshDirName("vespera-native-fresh-1234567890123456"));
        assertTrue(NativeExtractionSweep.isFreshDirName("vespera-native-fresh-7"));
        assertTrue(NativeExtractionSweep.isFreshDirName("vespera-native-4711"));
        assertTrue(NativeExtractionSweep.isFreshDirName("vespera-native-12345678901234567"));
        // A cache directory is 16 hex digits: a legacy 16-digit name could be one, so it is kept.
        assertFalse(NativeExtractionSweep.isFreshDirName("vespera-native-1234567890123456"));
        assertFalse(NativeExtractionSweep.isFreshDirName("vespera-native-abcdef0123456789"));
        assertFalse(NativeExtractionSweep.isFreshDirName("vespera-native-fresh-"));
        assertFalse(NativeExtractionSweep.isFreshDirName("vespera-native-notes"));
        assertFalse(NativeExtractionSweep.isFreshDirName("vespera-native-"));
        assertFalse(NativeExtractionSweep.isFreshDirName("vespera-native-4711.dll"));
    }

    @Test
    void aFreshDirectoryOfAnotherOwnerIsNeverRemoved() throws IOException {
        Path tmp = Files.createTempDirectory("vespera-native-sweep-owner-");
        try {
            Path fresh = Files.createDirectory(tmp.resolve(NativeExtractionSweep.FRESH_PREFIX + "1"));
            Path copy = Files.writeString(fresh.resolve("demo.so"), "x");
            Files.setLastModifiedTime(fresh, OLD);

            assertEquals(0, NativeExtractionSweep.sweep(tmp, NOW, SOMEONE_ELSE));
            assertTrue(Files.isRegularFile(copy));
            assertEquals(0, NativeExtractionSweep.sweep(tmp.resolve("missing"), NOW, SOMEONE_ELSE));
        } finally {
            deleteTree(tmp);
        }
    }

    @Test
    void aYoungFreshDirectoryIsLeftToItsJvm() throws IOException {
        Path tmp = Files.createTempDirectory("vespera-native-sweep-young-");
        try {
            Path fresh = Files.createDirectory(tmp.resolve(NativeExtractionSweep.FRESH_PREFIX + "2"));
            assertEquals(0, NativeExtractionSweep.sweep(tmp, NOW, me(tmp)));
            assertTrue(Files.isDirectory(fresh));
        } finally {
            deleteTree(tmp);
        }
    }

    @Test
    void byPathRemovalTakesOnlyOwnedOldFilesThenTheDirectory() throws IOException {
        Path fresh = Files.createTempDirectory("vespera-native-sweep-path-");
        try {
            Path copy = Files.writeString(fresh.resolve("demo.dll"), "x");
            Files.setLastModifiedTime(fresh, OLD);
            assertFalse(NativeExtractionSweep.removeByPath(fresh, SOMEONE_ELSE, NOW));
            assertTrue(Files.isRegularFile(copy));

            assertTrue(NativeExtractionSweep.removeByPath(fresh, me(fresh), NOW));
            assertFalse(Files.exists(fresh));
        } finally {
            deleteTree(fresh);
        }
    }

    @Test
    void byPathRemovalKeepsADirectoryThatStillHoldsSomething() throws IOException {
        Path fresh = Files.createTempDirectory("vespera-native-sweep-held-");
        try {
            Path held = Files.createDirectory(fresh.resolve("held"));
            Files.writeString(held.resolve("still-open"), "x");
            Files.setLastModifiedTime(fresh, OLD);
            assertThrows(IOException.class, () -> NativeExtractionSweep.removeByPath(fresh, me(fresh), NOW));
            assertTrue(Files.isDirectory(held));
        } finally {
            deleteTree(fresh);
        }
    }

    /** A symbolic link named like a fresh directory is never followed, and its target is untouched. */
    @Test
    void aSymlinkedFreshDirectoryIsNeverFollowed() throws IOException {
        assumeTrue(posix(), "symbolic links and SecureDirectoryStream: POSIX only");
        Path tmp = Files.createTempDirectory("vespera-native-sweep-link-");
        Path victim = Files.createTempDirectory("vespera-native-sweep-victim-");
        try {
            Path victimFile = Files.writeString(victim.resolve("victim.dll"), "keep me");
            Files.setLastModifiedTime(victimFile, OLD);
            Files.setLastModifiedTime(victim, OLD);
            Path link = Files.createSymbolicLink(tmp.resolve(NativeExtractionSweep.FRESH_PREFIX + "3"), victim);

            assertEquals(0, NativeExtractionSweep.sweep(tmp, NOW));
            assertTrue(Files.isSymbolicLink(link), "the link is not ours to remove either");
            assertEquals("keep me", Files.readString(victimFile));
        } finally {
            deleteTree(tmp);
            deleteTree(victim);
        }
    }

    /** A link inside an old fresh directory is never unlinked (only regular files are) and never followed. */
    @Test
    void aLinkInsideAFreshDirectoryIsNeverFollowedOrRemoved() throws IOException {
        assumeTrue(posix(), "symbolic links and SecureDirectoryStream: POSIX only");
        Path tmp = Files.createTempDirectory("vespera-native-sweep-inner-");
        Path victim = Files.createTempDirectory("vespera-native-sweep-inner-victim-");
        try {
            Path victimFile = Files.writeString(victim.resolve("victim.dll"), "keep me");
            Path fresh = Files.createDirectory(tmp.resolve(NativeExtractionSweep.FRESH_PREFIX + "4"));
            Path link = Files.createSymbolicLink(fresh.resolve("demo.dll"), victimFile);
            Files.setLastModifiedTime(fresh, OLD);

            assertEquals(0, NativeExtractionSweep.sweep(tmp, NOW));
            assertTrue(Files.isSymbolicLink(link));
            assertEquals("keep me", Files.readString(victimFile));
        } finally {
            deleteTree(tmp);
            deleteTree(victim);
        }
    }

    /** The owner the sweep runs as, for directories a test creates itself. */
    private static UserPrincipal me(Path dir) throws IOException {
        return Files.getOwner(dir);
    }

    private static void deleteTree(Path root) throws IOException {
        if (!Files.exists(root, LinkOption.NOFOLLOW_LINKS)) {
            return;
        }
        try (var paths = Files.walk(root)) {
            for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
                Files.deleteIfExists(path);
            }
        }
    }
}
