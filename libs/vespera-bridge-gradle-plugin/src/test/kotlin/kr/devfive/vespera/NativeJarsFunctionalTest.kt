package kr.devfive.vespera

import org.gradle.testkit.runner.GradleRunner
import org.gradle.testkit.runner.TaskOutcome
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.io.File
import java.util.zip.ZipFile

/**
 * TestKit: per-platform native jars (`vespera.nativeJarPlatforms`) and their
 * `vesperaNative` publication. Every platform is packaged from prebuilt files
 * (nothing is compiled), so the expectations are host-independent.
 */
class NativeJarsFunctionalTest {
    @TempDir
    lateinit var dir: File

    private val host = NativePlatform.hostPlatform()
    private val hostOs = host.substringBefore('-')

    private val platforms = mapOf(
        "linux-x86_64" to "libdemo.so",
        "linux-aarch64-musl" to "libdemo.so",
        "macos-aarch64" to "libdemo.dylib",
        "windows-x86_64" to "demo.dll",
    )

    private fun write(path: String, text: String = path) {
        val f = File(dir, path)
        f.parentFile.mkdirs()
        f.writeText(text)
    }

    private fun prebuilt(only: Map<String, String> = platforms) =
        only.forEach { (platform, lib) -> write("prebuilt/native/$platform/$lib", "lib for $platform") }

    private fun setUp(vesperaBlock: String, plugins: String = "`maven-publish`") {
        write("settings.gradle.kts", "rootProject.name = \"myapp-native\"\n")
        write(
            "build.gradle.kts",
            """
            plugins {
                $plugins
                id("kr.devfive.vespera-bridge")
            }
            group = "com.acme"
            version = "0.1.0"
            vespera {
                crateName.set("demo")
                bridgeVersion.set("0.0.0-test")
            $vesperaBlock
            }
            """.trimIndent() + if ("maven-publish" in plugins) {
                "\npublishing { repositories { maven { name = \"test\"; url = uri(\"repo\") } } }\n"
            } else {
                "\n"
            },
        )
    }

    private val platformList = platforms.keys.joinToString { "\"$it\"" }

    private fun run(vararg args: String) = GradleRunner.create()
        .withProjectDir(dir)
        .withPluginClasspath()
        .withArguments(*args, "--stacktrace")

    private fun fileEntries(jar: File): List<String> =
        ZipFile(jar).use { zip -> zip.entries().toList().filterNot { it.isDirectory }.map { it.name }.sorted() }

    @Test
    fun taskNamesAreCamelCasedPlatforms() {
        assertEquals("nativeJarLinuxX86_64", NativeJars.taskName("linux-x86_64"))
        assertEquals("nativeJarLinuxAarch64Musl", NativeJars.taskName("linux-aarch64-musl"))
        assertEquals("nativeJarWindowsX86_64", NativeJars.taskName("windows-x86_64"))
    }

    @Test
    fun requireLibraryNamesTheMissingFileAndTheFolderContents() {
        write("prebuilt/native/linux-x86_64/libother.so")
        val prebuilt = File(dir, "prebuilt")

        val wrongName = assertThrows(IllegalArgumentException::class.java) {
            NativeJars.requireLibrary("linux-x86_64", prebuilt, "libdemo.so")
        }
        assertTrue(wrongName.message!!.contains("libdemo.so not found"), wrongName.message)
        assertTrue(wrongName.message!!.contains("[libother.so]"), wrongName.message)
        assertThrows(IllegalArgumentException::class.java) {
            NativeJars.requireLibrary("macos-aarch64", prebuilt, null)
        }
        assertThrows(IllegalArgumentException::class.java) { NativeJars.requireLibrary("linux-x86_64", null, null) }
        NativeJars.requireLibrary("linux-x86_64", prebuilt, null)
        NativeJars.requireLibrary("linux-x86_64", prebuilt, "libother.so")
    }

    @Test
    fun publishesOneClassifierJarPerPlatform() {
        prebuilt()
        setUp(
            """
                bundleNative.set(false)
                nativeJarsDir.set(layout.projectDirectory.dir("prebuilt"))
                nativeJarPlatforms.addAll(listOf($platformList))
            """,
        )

        val result = run("publishAllPublicationsToTestRepository").build()

        val module = File(dir, "repo/com/acme/myapp-native/0.1.0")
        platforms.forEach { (platform, lib) ->
            assertEquals(TaskOutcome.SUCCESS, result.task(":${NativeJars.taskName(platform)}")?.outcome)
            val jar = File(module, "myapp-native-0.1.0-$platform.jar")
            assertTrue(jar.isFile, "missing $jar")
            assertEquals(listOf("META-INF/MANIFEST.MF", "native/$platform/$lib"), fileEntries(jar))
        }
        // Classifier artifacts only: no main jar, pom packaging.
        assertFalse(File(module, "myapp-native-0.1.0.jar").exists())
        val pom = File(module, "myapp-native-0.1.0.pom").readText()
        assertTrue(pom.contains("<packaging>pom</packaging>"), pom)
        assertTrue(pom.contains("<artifactId>myapp-native</artifactId>"), pom)
    }

    @Test
    fun extraNativeLibsIsTheDefaultSourceAndOnlyTheNamedPlatformIsPackaged() {
        prebuilt()
        setUp(
            """
                extraNativeLibs.set(layout.projectDirectory.dir("prebuilt"))
                nativeJarPlatforms.add("macos-aarch64")
            """,
            plugins = "base",
        )

        val result = run("nativeJars").build()

        assertEquals(TaskOutcome.SUCCESS, result.task(":nativeJarMacosAarch64")?.outcome)
        assertNull(result.task(":bundleNativeLib"), "no java plugin: nothing is bundled")
        val jar = File(dir, "build/libs/myapp-native-0.1.0-macos-aarch64.jar")
        assertEquals(listOf("META-INF/MANIFEST.MF", "native/macos-aarch64/libdemo.dylib"), fileEntries(jar))
    }

    @Test
    fun missingPlatformLibraryFailsWithItsPath() {
        prebuilt(mapOf("linux-x86_64" to "libdemo.so"))
        setUp(
            """
                nativeJarsDir.set(layout.projectDirectory.dir("prebuilt"))
                nativeJarPlatforms.addAll(listOf("linux-x86_64", "windows-x86_64"))
            """,
        )

        val result = run("nativeJars").buildAndFail()

        assertTrue(result.output.contains("Native jar 'windows-x86_64'"), result.output)
        assertTrue(result.output.contains("demo.dll"), result.output)
    }

    @Test
    fun invalidPlatformKeyIsRejected() {
        setUp("""    nativeJarPlatforms.add("solaris-sparc")""")

        val result = run("tasks").buildAndFail()

        assertTrue(result.output.contains("'solaris-sparc' must look like '<os>-<arch>'"), result.output)
    }

    @Test
    fun bundleNativeFalseNeedsNoLocalCdylib() {
        // A consumer app taking its library from native jars: no Cargo output exists.
        setUp(
            """
                cargoRoot.set(layout.projectDirectory.dir("rust"))
                bundleNative.set(false)
            """,
            plugins = "java",
        )

        val result = run("processResources").build()

        assertNull(result.task(":bundleNativeLib"), result.output)
        assertFalse(File(dir, "build/resources/main/native").exists())
    }

    @Test
    fun bundleNativeDefaultStillBundlesTheHostLibrary() {
        val hostLib = NativePlatform.mapLibraryName(hostOs, "demo")
        write("rust/target/release/$hostLib", "host")
        setUp("""    cargoRoot.set(layout.projectDirectory.dir("rust"))""", plugins = "java")

        val result = run("processResources").build()

        assertEquals(TaskOutcome.SUCCESS, result.task(":bundleNativeLib")?.outcome)
        assertEquals("host", File(dir, "build/resources/main/native/$host/$hostLib").readText())
    }

    @Test
    fun nativeJarsAreConfigurationCacheCompatible() {
        prebuilt()
        setUp(
            """
                bundleNative.set(false)
                nativeJarsDir.set(layout.projectDirectory.dir("prebuilt"))
                nativeJarPlatforms.addAll(listOf($platformList))
            """,
        )

        val first = run("publishAllPublicationsToTestRepository", "--configuration-cache").build()
        File(dir, "build").deleteRecursively()
        val second = run("publishAllPublicationsToTestRepository", "--configuration-cache").build()

        assertTrue(first.output.contains("Configuration cache entry stored"), first.output)
        assertTrue(second.output.contains("Reusing configuration cache"), second.output)
        assertFalse(second.output.contains("problems were found"), second.output)
        assertEquals(TaskOutcome.SUCCESS, second.task(":nativeJarWindowsX86_64")?.outcome)
        assertTrue(File(dir, "build/libs/myapp-native-0.1.0-linux-aarch64-musl.jar").isFile)
    }
}
