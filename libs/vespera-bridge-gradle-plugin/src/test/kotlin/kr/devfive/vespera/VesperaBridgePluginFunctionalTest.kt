package kr.devfive.vespera

import org.gradle.testkit.runner.GradleRunner
import org.gradle.testkit.runner.TaskOutcome
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assumptions.assumeTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File
import java.util.concurrent.TimeUnit

/** TestKit: bundling of host + cross-built + extra libraries into the jar resources. */
class VesperaBridgePluginFunctionalTest {
    @TempDir
    lateinit var dir: File

    private val host = NativePlatform.hostPlatform()
    private val hostOs = host.substringBefore('-')

    // Cross / prebuilt platforms that never coincide with the host's own
    // slot (the host build would overwrite them on an aarch64 runner).
    private val crossPlatform = if (host == "linux-aarch64") "linux-x86_64" else "linux-aarch64"
    private val crossTriple =
        if (host == "linux-aarch64") "x86_64-unknown-linux-gnu" else "aarch64-unknown-linux-gnu"
    private val extraPlatform = if (host == "macos-aarch64") "macos-x86_64" else "macos-aarch64"

    private fun write(path: String, text: String = path) {
        val f = File(dir, path)
        f.parentFile.mkdirs()
        f.writeText(text)
    }

    private fun setUp(
        vesperaBlock: String,
        crateName: String = "demo",
        bridgeVersion: String? = "0.0.0-test",
        extra: String = "",
    ) {
        write("settings.gradle.kts", "rootProject.name = \"consumer\"\n")
        val bridgeLine = bridgeVersion?.let { "bridgeVersion.set(\"$it\")" } ?: ""
        write(
            "build.gradle.kts",
            """
            plugins {
                java
                id("kr.devfive.vespera-bridge")
            }
            vespera {
                crateName.set("$crateName")
                cargoRoot.set(layout.projectDirectory.dir("rust"))
                $bridgeLine
            $vesperaBlock
            }
            $extra
            """.trimIndent(),
        )
    }

    private fun run(vararg args: String) = GradleRunner.create()
        .withProjectDir(dir)
        .withPluginClasspath()
        .withArguments(*args, "--stacktrace")

    private fun resource(path: String) = File(dir, "build/resources/main/native/$path")

    /** Stub the cargo invocation: print the planned commands instead of running them. */
    private val printCommands = """
                tasks.named<kr.devfive.vespera.CargoBuildTask>("cargoBuild") {
                    doFirst { println("CMDS=" + plannedCommands()); throw StopExecutionException() }
                }
    """

    @Test
    fun bundlesHostTargetsAndExtraLibraries() {
        val hostLib = NativePlatform.mapLibraryName(hostOs, "demo")
        write("rust/target/release/$hostLib", "host")
        write("rust/target/$crossTriple/release/libdemo.so", "cross")
        write("prebuilt/native/$extraPlatform/libdemo.dylib", "extra")
        write("prebuilt/native/$crossPlatform/libdemo.so", "stale extra")
        write("prebuilt/README.txt", "not bundled")
        setUp(
            """
                extraNativeLibs.set(layout.projectDirectory.dir("prebuilt"))
                targets.put("$crossPlatform", "$crossTriple")
            """,
        )

        val result = run("processResources").build()

        assertEquals(TaskOutcome.SUCCESS, result.task(":bundleNativeLib")?.outcome)
        assertEquals("host", resource("$host/$hostLib").readText())
        // The local cross build wins over the prebuilt copy of the same file.
        assertEquals("cross", resource("$crossPlatform/libdemo.so").readText())
        assertEquals("extra", resource("$extraPlatform/libdemo.dylib").readText())
        assertFalse(File(dir, "build/resources/main/README.txt").exists())
    }

    @Test
    fun hyphenatedCrateNameBundlesTheUnderscoreLibraryFile() {
        val hostLib = NativePlatform.mapLibraryName(hostOs, "rust_jni_demo")
        write("rust/target/release/$hostLib", "hyphen")
        setUp("", crateName = "rust-jni-demo")

        val result = run("processResources").build()

        assertEquals(TaskOutcome.SUCCESS, result.task(":bundleNativeLib")?.outcome)
        assertEquals("hyphen", resource("$host/$hostLib").readText())
    }

    @Test
    fun underscoreCrateNameStillBundles() {
        val hostLib = NativePlatform.mapLibraryName(hostOs, "rust_jni_demo")
        write("rust/target/release/$hostLib", "underscore")
        setUp("", crateName = "rust_jni_demo")

        run("processResources").build()

        assertEquals("underscore", resource("$host/$hostLib").readText())
    }

    @Test
    fun libNameOverridesTheLibraryFile() {
        val hostLib = NativePlatform.mapLibraryName(hostOs, "custom")
        write("rust/target/release/$hostLib", "custom")
        setUp("""    libName.set("custom")""", crateName = "rust-jni-demo")

        run("processResources").build()

        assertEquals("custom", resource("$host/$hostLib").readText())
    }

    @Test
    fun missingCrossBuildNamesTheTarget() {
        val hostLib = NativePlatform.mapLibraryName(hostOs, "demo")
        write("rust/target/release/$hostLib")
        setUp("""    targets.put("$crossPlatform", "$crossTriple")""")

        val result = run("processResources").buildAndFail()

        assertTrue(result.output.contains("target '$crossTriple'"), result.output)
    }

    @Test
    fun cargoBuildIsNeverUpToDate() {
        write("rust/Cargo.toml", "[workspace]\n")
        setUp(
            """
                packageName.set("demo")
                targets.put("$crossPlatform", "$crossTriple")
            """ + printCommands,
        )

        val first = run("cargoBuild").build()
        val second = run("cargoBuild").build()

        assertTrue(first.output.contains("--target, $crossTriple"), first.output)
        assertEquals(TaskOutcome.SUCCESS, second.task(":cargoBuild")?.outcome)
    }

    @Test
    fun hyphenatedCrateNameIsThePackageSpec() {
        setUp(printCommands, crateName = "rust-jni-demo")

        val result = run("cargoBuild").build()

        assertTrue(result.output.contains("CMDS=[[cargo, build, -p, rust-jni-demo, --release]]"), result.output)
    }

    @Test
    fun underscoreCrateNameResolvesTheHyphenatedPackageViaCargoMetadata() {
        assumeTrue(cargoAvailable(), "cargo is not on PATH")
        write(
            "rust/Cargo.toml",
            "[package]\nname = \"rust-jni-demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n" +
                "[lib]\ncrate-type = [\"cdylib\"]\n",
        )
        write("rust/src/lib.rs", "")
        setUp(printCommands, crateName = "rust_jni_demo")

        val result = run("cargoBuild").build()

        assertTrue(result.output.contains("CMDS=[[cargo, build, -p, rust-jni-demo, --release]]"), result.output)
    }

    @Test
    fun configurationCacheIsStoredAndReused() {
        val hostLib = NativePlatform.mapLibraryName(hostOs, "rust_jni_demo")
        write("rust/target/release/$hostLib", "cc")
        write("rust/target/$crossTriple/release/librust_jni_demo.so", "cross")
        write("prebuilt/native/$extraPlatform/librust_jni_demo.dylib", "extra")
        setUp(
            """
                packageName.set("rust-jni-demo")
                autoBuildCargo.set(true)
                extraNativeLibs.set(layout.projectDirectory.dir("prebuilt"))
                targets.put("$crossPlatform", "$crossTriple")
            """,
            crateName = "rust_jni_demo",
            // Untyped stub (no cargo run): a build-script lambda typed against
            // the plugin's CargoBuildTask is not loadable by TestKit's
            // injected-classpath CC serializer.
            extra = """
                tasks.named("cargoBuild") {
                    doFirst { println("CARGO-STUB"); throw StopExecutionException() }
                }
            """,
        )

        val first = run("processResources", "--configuration-cache").build()
        File(dir, "build").deleteRecursively()
        val second = run("processResources", "--configuration-cache").build()

        assertTrue(first.output.contains("Configuration cache entry stored"), first.output)
        assertTrue(second.output.contains("Reusing configuration cache"), second.output)
        assertFalse(second.output.contains("problems were found"), second.output)
        assertTrue(second.output.contains("CARGO-STUB"), second.output)
        assertEquals(TaskOutcome.SUCCESS, second.task(":cargoBuild")?.outcome)
        assertEquals(TaskOutcome.SUCCESS, second.task(":bundleNativeLib")?.outcome)
        assertEquals("cc", resource("$host/$hostLib").readText())
        assertEquals("extra", resource("$extraPlatform/librust_jni_demo.dylib").readText())
    }

    @Test
    fun bridgeVersionDefaultsToTheEmbeddedBridgeRelease() {
        val embedded = VesperaBridgePlugin.embeddedBridgeVersion()
        assertNotNull(embedded, "plugin build must embed the bridge version")
        setUp("", bridgeVersion = null)

        val result = run("dependencies", "--configuration", "compileClasspath").build()

        assertTrue(result.output.contains("kr.devfive:vespera-bridge:$embedded"), result.output)
    }

    @Test
    fun explicitBridgeVersionWins() {
        setUp("", bridgeVersion = "9.9.9-pinned")

        val result = run("dependencies", "--configuration", "compileClasspath").build()

        assertTrue(result.output.contains("kr.devfive:vespera-bridge:9.9.9-pinned"), result.output)
    }

    private fun cargoAvailable(): Boolean = try {
        val p = ProcessBuilder("cargo", "--version").redirectErrorStream(true).start()
        p.inputStream.readAllBytes()
        p.waitFor(30, TimeUnit.SECONDS) && p.exitValue() == 0
    } catch (e: Exception) {
        false
    }
}
