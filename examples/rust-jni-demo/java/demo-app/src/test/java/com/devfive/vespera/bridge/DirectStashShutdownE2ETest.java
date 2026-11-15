package com.devfive.vespera.bridge;

import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.Test;

/**
 * A runtime shutdown clears the DIRECT overflow stash after draining the
 * in-flight dispatches, possibly between {@code dispatchResumable0} and the
 * fetch / stream of its entry. That request must get the retryable {@code 503}
 * every other request gets during shutdown, not the {@code 500} "stash entry
 * expired" answer. Runs {@link StashShutdownProbe} in a child JVM: the
 * shutdown is irreversible for the process.
 */
class DirectStashShutdownE2ETest {

    @Test
    void stashEntryClearedByShutdownAnswers503() throws Exception {
        String java = ProcessHandle.current().info().command().orElse("java");
        List<String> command = new ArrayList<>(List.of(
                java, "-cp", System.getProperty("java.class.path")));
        String nativeDir = System.getProperty("vespera.native.dir");
        if (nativeDir != null) {
            command.add("-Dvespera.native.dir=" + nativeDir);
        }
        command.add(StashShutdownProbe.class.getName());
        // A file, not a pipe: reading a pipe to EOF would block until the
        // child exits, so the 60 s bound below would never apply.
        Path log = Files.createTempFile("vespera-stash-shutdown-probe", ".log");
        try {
            Process child = new ProcessBuilder(command)
                    .redirectErrorStream(true)
                    .redirectOutput(log.toFile())
                    .start();
            boolean exited = child.waitFor(60, TimeUnit.SECONDS);
            if (!exited) {
                child.destroyForcibly().waitFor(10, TimeUnit.SECONDS);
            }
            String text = new String(Files.readAllBytes(log), StandardCharsets.UTF_8);
            assertTrue(exited, "probe JVM did not exit within 60 s:\n" + text);
            assertTrue(text.contains("RESULT true 503 503"), text);
        } finally {
            Files.deleteIfExists(log);
        }
    }
}
