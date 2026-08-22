package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.io.InputStream;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.Comparator;
import java.util.UUID;
import org.junit.jupiter.api.Test;

class VesperaNativeLoaderTest {

    @Test
    void utilityConstructorIsPrivateButWellFormed() throws Exception {
        var constructor = VesperaNativeLoader.class.getDeclaredConstructor();
        constructor.setAccessible(true);
        assertInstanceOf(VesperaNativeLoader.class, constructor.newInstance());
    }

    @Test
    void absentBundledLibraryReportsResolvedResourcePath() {
        VesperaNativeLoader.BundledNativeAbsent error = assertThrows(
                VesperaNativeLoader.BundledNativeAbsent.class,
                () -> VesperaNativeLoader.loadBundled("absent_" + UUID.randomUUID()));

        assertTrue(error.getMessage().startsWith("Not found in JAR: native/"), error.getMessage());
        assertTrue(error.getMessage().contains(System.getProperty("os.arch").contains("64")
                ? "64"
                : System.getProperty("os.arch")), error.getMessage());
    }

    @Test
    void osArchAndLibraryMappingsCoverSupportedTriples() throws Exception {
        String oldOs = System.getProperty("os.name");
        String oldArch = System.getProperty("os.arch");
        try {
            assertDetection("Windows 11", "amd64", "windows", "x86_64", "demo.dll");
            assertDetection("Mac OS X", "arm64", "macos", "aarch64", "libdemo.dylib");
            assertDetection("Darwin", "aarch64", "windows", "aarch64", "demo.dll");
            assertDetection("FreeBSD", "riscv64", "linux", "riscv64", "libdemo.so");
            assertDetection("Linux", "x86_64", "linux", "x86_64", "libdemo.so");
        } finally {
            restoreProperty("os.name", oldOs);
            restoreProperty("os.arch", oldArch);
        }
    }

    @Test
    void digestHelperReadsWholeFileAndResetsDigest() throws Exception {
        Path temp = Files.createTempDirectory("vespera-native-loader-digest-");
        try {
            byte[] content = new byte[70 * 1024];
            for (int i = 0; i < content.length; i++) {
                content[i] = (byte) i;
            }
            Path file = temp.resolve("native.bin");
            Files.write(file, content);
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            digest.update("stale".getBytes(StandardCharsets.UTF_8));

            Method method = VesperaNativeLoader.class.getDeclaredMethod(
                    "digestOfFile", Path.class, MessageDigest.class);
            method.setAccessible(true);
            byte[] actual = (byte[]) method.invoke(null, file, digest);

            assertArrayEquals(MessageDigest.getInstance("SHA-256").digest(content), actual);
        } finally {
            deleteTree(temp);
        }
    }

    @Test
    void presentBundledResourceIsExtractedVerifiedAndRejectedOnlyAtSystemLoad() throws Exception {
        Path temp = Files.createTempDirectory("vespera-native-loader-resource-");
        String oldVerify = System.getProperty("vespera.native.verifyExtractedDigest");
        try {
            String os = invokeString(VesperaNativeLoader.class, "detectOs");
            String arch = invokeString(VesperaNativeLoader.class, "detectArch");
            String libraryName = "fabricated_" + UUID.randomUUID().toString().replace("-", "");
            String filename = invokeString(VesperaNativeLoader.class, "mapLibraryName", os, libraryName);
            Path resource = temp.resolve("native").resolve(os + "-" + arch).resolve(filename);
            Files.createDirectories(resource.getParent());
            Files.write(resource, "not a native library".getBytes(StandardCharsets.UTF_8));
            System.setProperty("vespera.native.verifyExtractedDigest", "true");

            try (ChildFirstLoader loader = new ChildFirstLoader(
                    new URL[] {temp.toUri().toURL(), mainClassesUrl()})) {
                Throwable failure = invokeChildLoadBundled(loader, libraryName);
                UnsatisfiedLinkError error = assertInstanceOf(UnsatisfiedLinkError.class, failure);
                assertTrue(error.getMessage() != null && !error.getMessage().isBlank());
            }
        } finally {
            restoreProperty("vespera.native.verifyExtractedDigest", oldVerify);
            deleteTree(temp);
        }
    }

    @Test
    void extractionIoFailureIsWrappedWithItsCause() throws Exception {
        Path temp = Files.createTempDirectory("vespera-native-loader-io-");
        try (ChildFirstLoader loader = new ChildFirstLoader(new URL[] {mainClassesUrl()}) {
            @Override
            public InputStream getResourceAsStream(String name) {
                if (name.startsWith("native/")) {
                    return new InputStream() {
                        @Override
                        public int read() throws IOException {
                            throw new IOException("fabricated read failure");
                        }
                    };
                }
                return super.getResourceAsStream(name);
            }
        }) {
            Throwable failure = invokeChildLoadBundled(loader, "fabricated_io");
            UnsatisfiedLinkError error = assertInstanceOf(UnsatisfiedLinkError.class, failure);
            assertTrue(error.getMessage().contains("fabricated read failure"), error.getMessage());
            assertInstanceOf(IOException.class, error.getCause());
        } finally {
            deleteTree(temp);
        }
    }

    private static void assertDetection(
            String osProperty,
            String archProperty,
            String expectedOs,
            String expectedArch,
            String expectedFilename) throws Exception {
        System.setProperty("os.name", osProperty);
        System.setProperty("os.arch", archProperty);
        assertEquals(expectedOs, invokeString(VesperaNativeLoader.class, "detectOs"));
        assertEquals(expectedArch, invokeString(VesperaNativeLoader.class, "detectArch"));
        assertEquals(expectedFilename,
                invokeString(VesperaNativeLoader.class, "mapLibraryName", expectedOs, "demo"));
    }

    private static String invokeString(Class<?> type, String name, Object... args) throws Exception {
        Class<?>[] parameterTypes = new Class<?>[args.length];
        for (int i = 0; i < args.length; i++) {
            parameterTypes[i] = String.class;
        }
        Method method = type.getDeclaredMethod(name, parameterTypes);
        method.setAccessible(true);
        return (String) method.invoke(null, args);
    }

    private static Throwable invokeChildLoadBundled(ClassLoader loader, String libraryName)
            throws Exception {
        Class<?> childLoader = Class.forName(
                "com.devfive.vespera.bridge.VesperaNativeLoader", true, loader);
        Method loadBundled = childLoader.getDeclaredMethod("loadBundled", String.class);
        loadBundled.setAccessible(true);
        InvocationTargetException invocation = assertThrows(
                InvocationTargetException.class,
                () -> loadBundled.invoke(null, libraryName));
        return invocation.getCause();
    }

    private static URL mainClassesUrl() {
        return VesperaBridge.class.getProtectionDomain().getCodeSource().getLocation();
    }

    private static void restoreProperty(String name, String value) {
        if (value == null) {
            System.clearProperty(name);
        } else {
            System.setProperty(name, value);
        }
    }

    private static void deleteTree(Path root) throws IOException {
        if (!Files.exists(root)) {
            return;
        }
        try (var paths = Files.walk(root)) {
            for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
                Files.delete(path);
            }
        }
    }

    private static class ChildFirstLoader extends URLClassLoader {
        ChildFirstLoader(URL[] urls) {
            super(urls, VesperaNativeLoaderTest.class.getClassLoader());
        }

        @Override
        protected Class<?> loadClass(String name, boolean resolve) throws ClassNotFoundException {
            if (name.equals("com.devfive.vespera.bridge.VesperaBridge")
                    || name.startsWith("com.devfive.vespera.bridge.VesperaNativeLoader")) {
                synchronized (getClassLoadingLock(name)) {
                    Class<?> loaded = findLoadedClass(name);
                    if (loaded == null) {
                        loaded = findClass(name);
                    }
                    if (resolve) {
                        resolveClass(loaded);
                    }
                    return loaded;
                }
            }
            return super.loadClass(name, resolve);
        }
    }
}
