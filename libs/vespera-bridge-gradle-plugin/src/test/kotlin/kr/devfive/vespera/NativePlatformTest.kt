package kr.devfive.vespera

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows
import org.junit.jupiter.params.ParameterizedTest
import org.junit.jupiter.params.provider.CsvSource

class NativePlatformTest {
    @ParameterizedTest
    @CsvSource(
        "Windows 11, windows",
        "Mac OS X, macos",
        // "darwin" contains "win": it must still map to macos.
        "Darwin, macos",
        "Linux, linux",
        "FreeBSD, linux",
    )
    fun detectsTheOsSegment(osName: String, expected: String) {
        assertEquals(expected, NativePlatform.detectOs(osName))
    }

    @ParameterizedTest
    @CsvSource("amd64, x86_64", "x86_64, x86_64", "aarch64, aarch64", "ARM64, aarch64", "riscv64, riscv64")
    fun detectsTheArchSegment(osArch: String, expected: String) {
        assertEquals(expected, NativePlatform.detectArch(osArch))
    }

    @ParameterizedTest
    @CsvSource(
        "release, , 'cargo build -p demo --release'",
        "dev, , 'cargo build -p demo'",
        "debug, aarch64-unknown-linux-gnu, 'cargo build -p demo --target aarch64-unknown-linux-gnu'",
        "bench, x86_64-pc-windows-msvc, 'cargo build -p demo --profile bench --target x86_64-pc-windows-msvc'",
    )
    fun buildsProfileAndTargetAwareCargoCommands(profile: String, triple: String?, expected: String) {
        assertEquals(expected, NativePlatform.cargoCommand("demo", profile, triple).joinToString(" "))
    }

    @Test
    fun resolvesCdylibPathsPerPlatform() {
        assertEquals("release/demo.dll", NativePlatform.cdylibRelativePath("windows", "demo", "release"))
        assertEquals("debug/libdemo.so", NativePlatform.cdylibRelativePath("linux", "demo", "dev"))
        assertEquals(
            "aarch64-apple-darwin/release/libdemo.dylib",
            NativePlatform.cdylibRelativePath("macos", "demo", "release", "aarch64-apple-darwin"),
        )
    }

    @Test
    fun plansTheHostBuildPlusEveryTarget() {
        val builds = NativePlatform.plannedBuilds(
            "windows-x86_64",
            "demo",
            "release",
            mapOf(
                "macos-aarch64" to "aarch64-apple-darwin",
                "linux-x86_64" to "x86_64-unknown-linux-gnu",
            ),
        )
        assertEquals(
            listOf(
                NativePlatform.NativeBuild("windows-x86_64", null, "release/demo.dll"),
                NativePlatform.NativeBuild(
                    "linux-x86_64", "x86_64-unknown-linux-gnu", "x86_64-unknown-linux-gnu/release/libdemo.so",
                ),
                NativePlatform.NativeBuild(
                    "macos-aarch64", "aarch64-apple-darwin", "aarch64-apple-darwin/release/libdemo.dylib",
                ),
            ),
            builds,
        )
    }

    @Test
    fun aTargetForTheHostPlatformReplacesTheHostBuild() {
        val builds = NativePlatform.plannedBuilds(
            "linux-x86_64", "demo", "release", mapOf("linux-x86_64" to "x86_64-unknown-linux-musl"),
        )
        assertEquals(listOf("x86_64-unknown-linux-musl"), builds.map { it.triple })
    }

    @Test
    fun rejectsMalformedTargets() {
        val badKey = assertThrows<IllegalArgumentException> {
            NativePlatform.plannedBuilds("linux-x86_64", "demo", "release", mapOf("darwin-arm64" to "x"))
        }
        assertTrue(badKey.message!!.contains("darwin-arm64"))
        assertThrows<IllegalArgumentException> {
            NativePlatform.plannedBuilds("linux-x86_64", "demo", "release", mapOf("linux-aarch64" to " "))
        }
    }

    @ParameterizedTest
    @CsvSource(
        "rust-jni-demo, , rust_jni_demo",
        "rust_jni_demo, , rust_jni_demo",
        "demo, , demo",
        "rust-jni-demo, custom_lib, custom_lib",
        "rust-jni-demo, ' ', rust_jni_demo",
    )
    fun derivesTheLibraryStemLikeCargo(crate: String, libName: String?, expected: String) {
        assertEquals(expected, NativePlatform.libraryStem(crate, libName))
    }

    @Test
    fun hyphenatedCratesMapToUnderscoreLibraryFiles() {
        val stem = NativePlatform.libraryStem("rust-jni-demo")
        assertEquals("release/rust_jni_demo.dll", NativePlatform.cdylibRelativePath("windows", stem, "release"))
        assertEquals("release/librust_jni_demo.so", NativePlatform.cdylibRelativePath("linux", stem, "release"))
    }

    @Test
    fun knowsThePackageSpecWithoutCargoWhenUnambiguous() {
        assertEquals("explicit", NativePlatform.knownPackageSpec("rust_jni_demo", "explicit"))
        assertEquals("rust-jni-demo", NativePlatform.knownPackageSpec("rust-jni-demo"))
        assertNull(NativePlatform.knownPackageSpec("rust_jni_demo"))
        assertNull(NativePlatform.knownPackageSpec("demo", " "))
    }

    private val metadata = """
        {"packages":[{"name":"vespera","version":"0.6.0","dependencies":[{"name":"serde"}],
        "targets":[{"kind":["lib"],"name":"vespera"}]},
        {"name":"rust-jni-demo","version":"0.1.0","dependencies":[{"name":"vespera"}],
        "targets":[{"kind":["cdylib","rlib"],"name":"rust_jni_demo"},{"kind":["bin"],"name":"rust-jni-demo"}]},
        {"name" : "under_score","targets":[{"name":"under_score"}]}],"workspace_members":[]}
    """.trimIndent()

    @ParameterizedTest
    @CsvSource(
        // The demo's underscore crateName resolves to its hyphenated package.
        "rust_jni_demo, rust-jni-demo",
        // A genuinely underscored package keeps its name.
        "under_score, under_score",
        "vespera, vespera",
        // Unknown names fall back unchanged (cargo reports the error).
        "missing_crate, missing_crate",
    )
    fun resolvesThePackageFromCargoMetadata(crate: String, expected: String) {
        assertEquals(expected, NativePlatform.resolvePackageName(crate, metadata))
    }

    @Test
    fun fallsBackToTheCrateNameWithoutMetadata() {
        assertEquals("rust_jni_demo", NativePlatform.resolvePackageName("rust_jni_demo", null))
        assertEquals("rust_jni_demo", NativePlatform.resolvePackageName("rust_jni_demo", ""))
    }
}
