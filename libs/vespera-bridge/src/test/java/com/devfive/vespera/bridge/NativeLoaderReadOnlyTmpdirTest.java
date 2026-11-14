package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.Comparator;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.stream.Stream;
import org.junit.jupiter.api.Test;

/**
 * {@code vespera.native.dir} must be enough on hosts whose
 * {@code java.io.tmpdir} is missing or read-only (read-only root file system
 * containers): extraction — including the current-user ownership probe —
 * stays inside the configured directory. {@code Files.createTempFile}
 * resolves {@code java.io.tmpdir} once per JVM, so the scenario runs in a
 * child JVM started with a {@code java.io.tmpdir} that does not exist.
 */
class NativeLoaderReadOnlyTmpdirTest {

    /** Child-JVM entry point: extract a fake library into {@code args[0]} and print its path. */
    public static void main(String[] args) throws Exception {
        byte[] library = "not really a library".getBytes(StandardCharsets.UTF_8);
        Path extracted = VesperaNativeLoader.extract("native/test/libdemo.so", "libdemo.so",
                Paths.get(args[0]), () -> new ByteArrayInputStream(library));
        System.out.println("EXTRACTED=" + extracted.toAbsolutePath());
    }

    @Test
    void configuredNativeDirWorksWithoutAWritableTmpdir() throws Exception {
        Path root = Files.createTempDirectory("vespera-native-rotmp-");
        try {
            Path base = root.resolve("native");
            Path missingTmp = root.resolve("no-such-tmp");
            Path argFile = root.resolve("java.args");
            // An argument file: the test classpath can exceed the Windows command-line limit.
            Files.writeString(argFile, "-cp\n\"" + System.getProperty("java.class.path")
                    .replace('\\', '/') + "\"\n");
            String java = Paths.get(System.getProperty("java.home"), "bin", "java").toString();
            Process child = new ProcessBuilder(List.of(java, "@" + argFile,
                    "-Djava.io.tmpdir=" + missingTmp, NativeLoaderReadOnlyTmpdirTest.class.getName(),
                    base.toString()))
                    .redirectErrorStream(true)
                    .start();
            String output = new String(child.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
            assertTrue(child.waitFor(60, TimeUnit.SECONDS), "child JVM timed out");
            assertEquals(0, child.exitValue(), output);
            String line = output.lines().filter(l -> l.startsWith("EXTRACTED=")).findFirst().orElseThrow();
            Path extracted = Paths.get(line.substring("EXTRACTED=".length()));
            assertTrue(extracted.startsWith(base.toAbsolutePath()),
                    "extracted into the configured, reusable directory: " + output);
            assertTrue(Files.isRegularFile(extracted));
            assertTrue(Files.notExists(missingTmp), "java.io.tmpdir was never needed");
        } finally {
            deleteTree(root);
        }
    }

    private static void deleteTree(Path root) throws Exception {
        if (!Files.exists(root)) {
            return;
        }
        try (Stream<Path> walk = Files.walk(root)) {
            for (Path p : walk.sorted(Comparator.reverseOrder()).toList()) {
                File f = p.toFile();
                f.setWritable(true);
                Files.deleteIfExists(p);
            }
        }
    }
}
