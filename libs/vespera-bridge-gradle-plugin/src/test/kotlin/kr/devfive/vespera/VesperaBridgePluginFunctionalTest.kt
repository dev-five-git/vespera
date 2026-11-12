package kr.devfive.vespera

import org.gradle.testkit.runner.GradleRunner
import org.gradle.testkit.runner.TaskOutcome
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File

/** TestKit: bundling of host + cross-built + extra libraries into the jar resources. */
class VesperaBridgePluginFunctionalTest {
    @TempDir
    lateinit var dir: File

    private val host = NativePlatform.detectOs(System.getProperty("os.name", "")) +
        "-" + NativePlatform.detectArch(System.getProperty("os.arch", ""))

    private fun write(path: String, text: String = path) {
        val f = File(dir, path)
        f.parentFile.mkdirs()
        f.writeText(text)
    }

    private fun setUp(vesperaBlock: String) {
        write("settings.gradle.kts", "rootProject.name = \"consumer\"\n")
        write(
            "build.gradle.kts",
            """
            plugins {
                java
                id("kr.devfive.vespera-bridge")
            }
            vespera {
                crateName.set("demo")
                cargoRoot.set(layout.projectDirectory.dir("rust"))
                bridgeVersion.set("0.0.0-test")
            $vesperaBlock
            }
            """.trimIndent(),
        )
    }

    private fun run(vararg args: String) = GradleRunner.create()
        .withProjectDir(dir)
        .withPluginClasspath()
        .withArguments(*args, "--stacktrace")

    private fun resource(path: String) = File(dir, "build/resources/main/native/$path")

    @Test
    fun bundlesHostTargetsAndExtraLibraries() {
        val hostLib = NativePlatform.mapLibraryName(host.substringBefore('-'), "demo")
        write("rust/target/release/$hostLib", "host")
        write("rust/target/aarch64-unknown-linux-gnu/release/libdemo.so", "cross")
        write("prebuilt/native/macos-aarch64/libdemo.dylib", "extra")
        write("prebuilt/native/linux-aarch64/libdemo.so", "stale extra")
        write("prebuilt/README.txt", "not bundled")
        setUp(
            """
                extraNativeLibs.set(layout.projectDirectory.dir("prebuilt"))
                targets.put("linux-aarch64", "aarch64-unknown-linux-gnu")
            """,
        )

        val result = run("processResources").build()

        assertEquals(TaskOutcome.SUCCESS, result.task(":bundleNativeLib")?.outcome)
        assertEquals("host", resource("$host/$hostLib").readText())
        // The local cross build wins over the prebuilt copy of the same file.
        assertEquals("cross", resource("linux-aarch64/libdemo.so").readText())
        assertEquals("extra", resource("macos-aarch64/libdemo.dylib").readText())
        assertFalse(File(dir, "build/resources/main/README.txt").exists())
    }

    @Test
    fun missingCrossBuildNamesTheTarget() {
        val hostLib = NativePlatform.mapLibraryName(host.substringBefore('-'), "demo")
        write("rust/target/release/$hostLib")
        setUp("""    targets.put("linux-aarch64", "aarch64-unknown-linux-gnu")""")

        val result = run("processResources").buildAndFail()

        assertTrue(result.output.contains("target 'aarch64-unknown-linux-gnu'"), result.output)
    }

    @Test
    fun cargoBuildIsNeverUpToDate() {
        // A fake `cargo` is not portable; inspect the task's planned commands instead.
        write("rust/Cargo.toml", "[workspace]\n")
        setUp(
            """
                targets.put("linux-aarch64", "aarch64-unknown-linux-gnu")
                tasks.named<kr.devfive.vespera.CargoBuildTask>("cargoBuild") {
                    doFirst { println("CMDS=" + commands.get()); throw StopExecutionException() }
                }
            """,
        )

        val first = run("cargoBuild").build()
        val second = run("cargoBuild").build()

        assertTrue(first.output.contains("--target, aarch64-unknown-linux-gnu"), first.output)
        assertEquals(TaskOutcome.SUCCESS, second.task(":cargoBuild")?.outcome)
    }
}
