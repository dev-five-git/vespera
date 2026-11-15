package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assumptions.assumeTrue;

import java.io.IOException;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.nio.file.attribute.PosixFilePermissions;
import java.nio.file.attribute.UserPrincipal;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

/**
 * The current-user answer behind the extraction-directory ownership checks
 * must not come from the directory being verified while a trusted source is
 * available, and an answer that did come from it must never be cached.
 */
class CurrentUserTest {

    private static final UserPrincipal MALLORY = () -> "mallory";

    /** A scripted {@link CurrentUser.Sources}; {@code null} answers mean "unavailable". */
    private static class Scripted implements CurrentUser.Sources {
        UserPrincipal process;
        UserPrincipal tmpdir;
        UserPrincipal userName;
        final List<Path> dirProbes = new ArrayList<>();
        final List<UserPrincipal> dirAnswers = new ArrayList<>();
        final AtomicInteger trustedCalls = new AtomicInteger();
        Runnable duringDirProbe = () -> {};

        @Override
        public UserPrincipal processOwner() {
            trustedCalls.incrementAndGet();
            return process;
        }

        @Override
        public UserPrincipal tmpdirProbeOwner() throws IOException {
            if (tmpdir == null) {
                throw new IOException("java.io.tmpdir is read-only");
            }
            return tmpdir;
        }

        @Override
        public UserPrincipal userNameLookup() throws IOException {
            if (userName == null) {
                throw new UnsupportedOperationException("no principal lookup");
            }
            return userName;
        }

        @Override
        public UserPrincipal probeOwnerIn(Path dir) {
            dirProbes.add(dir);
            duringDirProbe.run();
            return dirAnswers.get(Math.min(dirProbes.size(), dirAnswers.size()) - 1);
        }
    }

    private Path dir;

    @BeforeEach
    void setUp() throws IOException {
        CurrentUser.resetCache();
        dir = Files.createTempDirectory("vespera-current-user-");
        if (posix()) {
            Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwx------"));
        }
    }

    @AfterEach
    void tearDown() throws IOException {
        CurrentUser.sources = CurrentUser.SYSTEM;
        CurrentUser.resetCache();
        if (posix() && Files.exists(dir)) {
            Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwx------"));
        }
        Files.deleteIfExists(dir);
    }

    private static boolean posix() {
        return FileSystems.getDefault().supportedFileAttributeViews().contains("posix");
    }

    private static Scripted install(Scripted scripted) {
        CurrentUser.sources = scripted;
        return scripted;
    }

    @Test
    void processOwnerWinsAndIsCachedWithoutProbingTheDirectory() throws Exception {
        UserPrincipal me = () -> "me";
        Scripted s = install(new Scripted());
        s.process = me;
        s.dirAnswers.add(MALLORY);
        assertSame(me, CurrentUser.of(dir));
        assertSame(me, CurrentUser.of(dir));
        assertEquals(1, s.trustedCalls.get(), "cached after the first answer");
        assertTrue(s.dirProbes.isEmpty(), "the directory being verified is never probed");
    }

    @Test
    void tmpdirProbeThenUserNameLookupPrecedeTheDirectory() throws Exception {
        UserPrincipal me = () -> "me";
        Scripted s = install(new Scripted());
        s.tmpdir = me;
        s.dirAnswers.add(MALLORY);
        assertSame(me, CurrentUser.of(dir));

        CurrentUser.resetCache();
        Scripted lookup = install(new Scripted());
        lookup.userName = me;
        lookup.dirAnswers.add(MALLORY);
        assertSame(me, CurrentUser.of(dir));
        assertTrue(s.dirProbes.isEmpty() && lookup.dirProbes.isEmpty());
    }

    @Test
    void directoryProbeIsTheLastResortAndNeverCached() throws Exception {
        UserPrincipal first = () -> "first";
        UserPrincipal second = () -> "second";
        Scripted s = install(new Scripted());
        s.dirAnswers.add(first);
        s.dirAnswers.add(second);
        assertSame(first, CurrentUser.of(dir));
        assertSame(second, CurrentUser.of(dir), "an unverified directory's answer is not cached");
        assertEquals(List.of(dir, dir), s.dirProbes);
    }

    @Test
    void lastResortRefusesAGroupOrOtherWritableDirectory() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwxrwxrwx"));
        Scripted s = install(new Scripted());
        s.dirAnswers.add(MALLORY);
        IOException e = assertThrows(IOException.class, () -> CurrentUser.of(dir));
        assertTrue(s.dirProbes.isEmpty(), "not probed");
        assertTrue(List.of(e.getSuppressed()).stream()
                .anyMatch(x -> x instanceof VesperaNativeLoader.UnsafeDirectoryException), e.toString());
    }

    @Test
    void lastResortRejectsADirectoryWhoseModeChangesDuringTheProbe() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Scripted s = install(new Scripted());
        s.dirAnswers.add(MALLORY);
        s.duringDirProbe = () -> {
            try {
                Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwxrwxrwx"));
            } catch (IOException e) {
                throw new AssertionError(e);
            }
        };
        assertThrows(IOException.class, () -> CurrentUser.of(dir));
    }

    @Test
    void lastResortRejectsADirectorySwappedDuringTheProbe() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Scripted s = install(new Scripted());
        s.dirAnswers.add(MALLORY);
        s.duringDirProbe = () -> {
            try {
                Path aside = Files.createTempDirectory("vespera-current-user-aside-");
                Files.delete(dir);
                Files.move(aside, dir);
                Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwx------"));
            } catch (IOException e) {
                throw new AssertionError(e);
            }
        };
        IOException e = assertThrows(IOException.class, () -> CurrentUser.of(dir));
        assertTrue(List.of(e.getSuppressed()).stream()
                .anyMatch(x -> String.valueOf(x.getMessage()).contains("changed while probing")), e.toString());
    }

    @Test
    void lastResortRefusesASymbolicLink() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Path link = dir.resolveSibling(dir.getFileName() + "-link");
        Files.createSymbolicLink(link, dir);
        try {
            Scripted s = install(new Scripted());
            s.dirAnswers.add(MALLORY);
            assertThrows(IOException.class, () -> CurrentUser.of(link));
            assertTrue(s.dirProbes.isEmpty());
        } finally {
            Files.deleteIfExists(link);
        }
    }

    /**
     * The round-9 scenario: the directory being verified answers the probe
     * with an attacker-owned file. With a trusted source available the
     * ownership check never sees that answer.
     */
    @Test
    void anAttackerOwnedProbeCannotVouchForTheDirectory() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        UserPrincipal owner = Files.getOwner(dir, LinkOption.NOFOLLOW_LINKS);
        Scripted s = install(new Scripted());
        s.process = owner;
        s.dirAnswers.add(MALLORY);
        VesperaNativeLoader.requirePrivateDirectory(dir);
        assertTrue(s.dirProbes.isEmpty());

        CurrentUser.resetCache();
        Scripted attacked = install(new Scripted());
        attacked.process = MALLORY;
        assertThrows(VesperaNativeLoader.UnsafeDirectoryException.class,
                () -> VesperaNativeLoader.requirePrivateDirectory(dir),
                "a directory not owned by the trusted current user is rejected");
    }

    @Test
    void groupWritableDirectoryIsRejectedBeforeTheCurrentUserIsWorkedOut() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwxrwx---"));
        Scripted s = install(new Scripted());
        s.dirAnswers.add(MALLORY);
        assertThrows(VesperaNativeLoader.UnsafeDirectoryException.class,
                () -> VesperaNativeLoader.requirePrivateDirectory(dir));
        assertEquals(0, s.trustedCalls.get());
        assertTrue(s.dirProbes.isEmpty());
    }

    @Test
    void systemSourcesAgreeWithTheOwnerOfAFileThisProcessCreates() throws Exception {
        Path probe = Files.createTempFile(dir, "owner-", ".probe");
        UserPrincipal fileOwner;
        try {
            fileOwner = Files.getOwner(probe, LinkOption.NOFOLLOW_LINKS);
        } finally {
            Files.delete(probe);
        }
        UserPrincipal process = CurrentUser.SYSTEM.processOwner();
        if (System.getProperty("os.name", "").toLowerCase(Locale.ROOT).startsWith("linux")) {
            assertEquals(fileOwner, process);
        } else {
            assertNull(process);
        }
        assertEquals(fileOwner, CurrentUser.SYSTEM.tmpdirProbeOwner());
        assertEquals(fileOwner, CurrentUser.SYSTEM.probeOwnerIn(dir));
        try (var listing = Files.list(dir)) {
            assertEquals(0, listing.count(), "probe files are deleted");
        }
        CurrentUser.resetCache();
        assertEquals(fileOwner, VesperaNativeLoader.currentUser(dir));
    }

    @Test
    void userNameLookupIgnoresUnknownAndPlaceholderNames() throws Exception {
        String saved = System.getProperty("user.name");
        try {
            System.setProperty("user.name", "?");
            assertNull(CurrentUser.SYSTEM.userNameLookup());
            System.setProperty("user.name", " ");
            assertNull(CurrentUser.SYSTEM.userNameLookup());
            System.setProperty("user.name", "vespera-no-such-user-" + System.nanoTime());
            assertNull(CurrentUser.SYSTEM.userNameLookup());
            System.setProperty("user.name", saved);
            UserPrincipal resolved = CurrentUser.SYSTEM.userNameLookup();
            assertNotNull(resolved);
            assertNotEquals(MALLORY.getName(), resolved.getName());
        } finally {
            System.setProperty("user.name", saved);
        }
    }
}
