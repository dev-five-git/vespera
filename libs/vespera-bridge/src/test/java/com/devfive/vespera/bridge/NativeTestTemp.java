package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assumptions.assumeTrue;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.AclFileAttributeView;
import java.nio.file.attribute.UserPrincipal;
import java.util.Locale;

/**
 * Temp base directories for the native-loader tests that expect the loader to
 * reuse a directory.
 *
 * <p>On Windows a new directory inherits the ACL of {@code %TEMP%}. Some
 * machines carry stale domain-account SIDs there with write rights (for
 * example {@code Modify} for {@code S-1-5-21-...}); {@link VesperaNativeLoader}
 * then correctly refuses to reuse the directory and extracts into a fresh one.
 * Those tests are skipped on such a machine, with the reason, instead of
 * failing on the intended behaviour.
 */
final class NativeTestTemp {

    private NativeTestTemp() {
    }

    /** A fresh temp directory; skips the calling test when the loader could not reuse it. */
    static Path reusableBase(String prefix) throws IOException {
        Path base = Files.createTempDirectory(prefix);
        assumeReusable(base);
        return base;
    }

    /** Skip unless {@code base} passes the loader's own ACL rule (Windows only). */
    static void assumeReusable(Path base) throws IOException {
        if (!System.getProperty("os.name", "").toLowerCase(Locale.ROOT).startsWith("windows")) {
            return;
        }
        UserPrincipal owner = Files.getOwner(base);
        AclFileAttributeView acl = Files.getFileAttributeView(base, AclFileAttributeView.class);
        String problem = VesperaNativeLoader.aclProblem(acl.getAcl(),
                p -> p.equals(owner) || VesperaNativeLoader.isWellKnownAdmin(p));
        assumeTrue(problem == null,
                "temp directory inherits write rights of another principal (" + problem
                        + "); the loader correctly refuses to reuse it on this machine");
    }
}
