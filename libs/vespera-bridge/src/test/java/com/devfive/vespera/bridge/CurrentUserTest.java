package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
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
        final AtomicInteger trustedCalls = new AtomicInteger();

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
        assertSame(me, CurrentUser.of());
        assertSame(me, CurrentUser.of());
        assertEquals(1, s.trustedCalls.get(), "cached after the first answer");
    }

    @Test
    void tmpdirProbeThenUserNameLookupPrecedeTheDirectory() throws Exception {
        UserPrincipal me = () -> "me";
        Scripted s = install(new Scripted());
        s.tmpdir = me;
        assertSame(me, CurrentUser.of());

        CurrentUser.resetCache();
        Scripted lookup = install(new Scripted());
        lookup.userName = me;
        assertSame(me, CurrentUser.of());
    }

    @Test
    void anUntrustedTmpdirFallsThroughToTheUserNameLookupNotTheDirectory() throws Exception {
        UserPrincipal me = () -> "me";
        // tmpdirProbeOwner answers null for an untrusted java.io.tmpdir.
        Scripted s = install(new Scripted() {
            @Override
            public UserPrincipal tmpdirProbeOwner() {
                return null;
            }
        });
        s.userName = me;
        assertSame(me, CurrentUser.of());
    }

    /**
     * No trusted source (no procfs owner, no usable temp directory, no
     * resolvable {@code user.name}): the lookup fails closed and never probes
     * the directory being verified, so the loader cannot accept a directory
     * on an answer that directory supplied.
     */
    @Test
    void withoutATrustedSourceTheLookupFailsClosedWithTheDirectoryAdvice() {
        install(new Scripted());
        IOException e = assertThrows(IOException.class, CurrentUser::of);
        assertTrue(e.getMessage().contains("-Dvespera.native.dir"), e.getMessage());
        assertThrows(IOException.class, CurrentUser::of, "a failed answer is not cached");
    }

    @Test
    void anOwnershipCheckFailsClosedWithoutATrustedSource() {
        assumeTrue(posix(), "POSIX file permissions required");
        install(new Scripted());
        assertThrows(IOException.class, () -> VesperaNativeLoader.requirePrivateDirectory(dir),
                "the directory being verified is not accepted on its own answer");
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
        VesperaNativeLoader.requirePrivateDirectory(dir);

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
        assertThrows(VesperaNativeLoader.UnsafeDirectoryException.class,
                () -> VesperaNativeLoader.requirePrivateDirectory(dir));
        assertEquals(0, s.trustedCalls.get());
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
        CurrentUser.resetCache();
        assertEquals(fileOwner, VesperaNativeLoader.currentUser());
    }

    @Test
    void aTmpdirAnotherUserOwnsIsNotTrustedEvenWhenNoOneElseCanWriteIt() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Path owned = Files.createTempDirectory(dir, "owned-");
        try {
            Files.setPosixFilePermissions(owned, PosixFilePermissions.fromString("rwxr-xr-x"));
            UserPrincipal owner = Files.getOwner(owned);
            // Owned by this process: trusted.
            assertTrue(CurrentUser.trustedTmpdir(owned, owner));
            // Owned by mallory at 0755: the owner can rename the probe before it is read.
            assertFalse(CurrentUser.trustedTmpdir(owned, MALLORY));
            // The process identity is unknown: nothing can be verified.
            assertFalse(CurrentUser.trustedTmpdir(owned, null));
        } finally {
            Files.deleteIfExists(owned);
        }
    }

    @Test
    void aWorldWritableNonStickyTmpdirIsNotProbed() throws Exception {
        assumeTrue(posix(), "POSIX file permissions required");
        Path shared = Files.createTempDirectory(dir, "shared-");
        Files.setPosixFilePermissions(shared, PosixFilePermissions.fromString("rwxrwxrwx"));
        String saved = System.getProperty("java.io.tmpdir");
        try {
            System.setProperty("java.io.tmpdir", shared.toString());
            // Another local user could replace the probe here: no probe owner is trusted.
            assertNull(CurrentUser.SYSTEM.tmpdirProbeOwner());
            try (var listing = Files.list(shared)) {
                assertEquals(0, listing.count(), "no probe was created in the shared directory");
            }
        } finally {
            System.setProperty("java.io.tmpdir", saved);
            Files.deleteIfExists(shared);
        }
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
            // The lookup resolves the requested account. Windows may qualify the name with a
            // domain or host (DOMAIN\\user) and folds case, so compare the account part only.
            String account = resolved.getName().substring(resolved.getName().lastIndexOf('\\') + 1);
            assertTrue(account.equalsIgnoreCase(saved),
                    "resolved " + resolved.getName() + " for user.name " + saved);
        } finally {
            System.setProperty("user.name", saved);
        }
    }
}
