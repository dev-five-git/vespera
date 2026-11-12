package kr.devfive.vespera

import org.junit.jupiter.api.Assertions.assertEquals
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

    @Test
    fun excludesBuildOutputAndVcsFromCargoInputs() {
        assertEquals(
            listOf("**/target/**", "**/node_modules/**", "**/.git/**"),
            NativePlatform.CARGO_INPUT_EXCLUDES,
        )
    }
}
