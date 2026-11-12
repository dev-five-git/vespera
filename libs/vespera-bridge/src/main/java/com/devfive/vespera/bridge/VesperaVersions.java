package com.devfive.vespera.bridge;

import java.io.IOException;
import java.io.InputStream;
import java.lang.System.Logger.Level;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import java.util.Properties;
import java.util.function.Supplier;

/**
 * Bridge JAR &harr; native library version check run by
 * {@link VesperaBridge#init(String)}.
 *
 * <p>The JNI symbol set, the {@code configure*0} signatures and the binary wire
 * format are only stable within one {@code major.minor} release line, so the
 * bridge compares its own version (embedded at build time) with the
 * {@code CARGO_PKG_VERSION} the native library reports through
 * {@code nativeVersion0()}:
 * <ul>
 *   <li>same {@code major.minor}: accepted (patch levels may differ);</li>
 *   <li>different {@code major.minor}: {@link IllegalStateException} (the
 *       default {@code strict} mode), or only a WARNING with
 *       {@code -Dvespera.native.versionCheck=warn}; {@code off} skips the
 *       check;</li>
 *   <li>a native library that predates {@code nativeVersion0()} (≤ 0.5.x) or
 *       a bridge whose version is unknown: a WARNING, never a failure.</li>
 * </ul>
 *
 * <p>Package-private: an internal collaborator of {@link VesperaBridge}.
 */
final class VesperaVersions {

    private static final System.Logger LOG = System.getLogger(VesperaBridge.class.getName());

    /** System property selecting {@code strict} (default), {@code warn} or {@code off}. */
    static final String CHECK_PROPERTY = "vespera.native.versionCheck";

    private static final String RESOURCE = "vespera-bridge.properties";

    private static final String BRIDGE_VERSION = loadBridgeVersion();

    private static volatile String nativeVersion;

    private VesperaVersions() {}

    /** Version of this bridge JAR, or {@code null} when it was not embedded (e.g. an IDE build). */
    static String bridgeVersion() {
        return BRIDGE_VERSION;
    }

    /** Version reported by the loaded native library, or {@code null} (not loaded / too old). */
    static String nativeVersion() {
        return nativeVersion;
    }

    /** Check the freshly loaded native library (see the class documentation). */
    static void verify(Supplier<byte[]> nativeVersionHook) {
        String mode = System.getProperty(CHECK_PROPERTY, "strict").trim().toLowerCase(Locale.ROOT);
        String reported = readNativeVersion(nativeVersionHook);
        nativeVersion = reported;
        String problem = mismatch(BRIDGE_VERSION, reported);
        if (problem == null || mode.equals("off")) {
            return;
        }
        boolean comparable = BRIDGE_VERSION != null && reported != null;
        if (comparable && !mode.equals("warn")) {
            throw new IllegalStateException(problem + " Rebuild the native library against the "
                    + "matching vespera crate (or use the matching vespera-bridge version). Set -D"
                    + CHECK_PROPERTY + "=warn to start anyway.");
        }
        LOG.log(Level.WARNING, "vespera: " + problem);
    }

    /** Description of a version problem, or {@code null} when the versions are compatible. */
    static String mismatch(String bridge, String nativeLib) {
        if (nativeLib == null) {
            return "the loaded native library does not report its version (built against "
                    + "vespera <= 0.5); it must come from the same release as vespera-bridge "
                    + (bridge == null ? "" : bridge + " ") + "or dispatch may fail.";
        }
        if (bridge == null) {
            return null;
        }
        String bridgeLine = majorMinor(bridge);
        String nativeLine = majorMinor(nativeLib);
        if (bridgeLine != null && bridgeLine.equals(nativeLine)) {
            return null;
        }
        return "vespera-bridge " + bridge + " and the native library (vespera " + nativeLib
                + ") come from different release lines (" + bridgeLine + " vs " + nativeLine
                + "): the JNI symbols and the wire format are only compatible within one "
                + "major.minor line.";
    }

    /** {@code "0.6"} for {@code "0.6.1-rc.1"}, or {@code null} when not a {@code x.y...} version. */
    static String majorMinor(String version) {
        if (version == null) {
            return null;
        }
        String[] parts = version.trim().split("[.+-]", 3);
        if (parts.length < 2 || !isNumber(parts[0]) || !isNumber(parts[1])) {
            return null;
        }
        return Integer.parseInt(parts[0]) + "." + Integer.parseInt(parts[1]);
    }

    private static boolean isNumber(String s) {
        if (s.isEmpty() || s.length() > 9) {
            return false;
        }
        for (int i = 0; i < s.length(); i++) {
            if (s.charAt(i) < '0' || s.charAt(i) > '9') {
                return false;
            }
        }
        return true;
    }

    private static String readNativeVersion(Supplier<byte[]> hook) {
        try {
            byte[] bytes = hook.get();
            if (bytes == null || bytes.length == 0) {
                return null;
            }
            return new String(bytes, StandardCharsets.UTF_8).trim();
        } catch (UnsatisfiedLinkError olderNativeLibrary) {
            return null;
        }
    }

    private static String loadBridgeVersion() {
        try (InputStream in = VesperaVersions.class.getResourceAsStream(RESOURCE)) {
            if (in == null) {
                return null;
            }
            Properties props = new Properties();
            props.load(in);
            String version = props.getProperty("version");
            // An unexpanded template (resources not processed by Gradle).
            return version == null || version.isBlank() || version.contains("$") ? null : version.trim();
        } catch (IOException | IllegalArgumentException unreadable) {
            return null;
        }
    }
}
