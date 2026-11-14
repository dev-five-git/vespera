package kr.devfive.vespera

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
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
    fun aTargetBundledIntoTheHostFolderReplacesTheHostBuild() {
        val builds = NativePlatform.plannedBuilds(
            "linux-x86_64", "demo", "release", mapOf("linux-x86_64" to "x86_64-unknown-linux-gnu"),
        )
        assertEquals(
            listOf(
                NativePlatform.NativeBuild(
                    "linux-x86_64", "x86_64-unknown-linux-gnu", "x86_64-unknown-linux-gnu/release/libdemo.so",
                ),
            ),
            builds,
        )
    }

    @Test
    fun aHostKeyMappedToMuslKeepsTheHostBuild() {
        // The musl triple is bundled where the bridge looks on musl
        // (native/linux-x86_64-musl/), so native/linux-x86_64/ — where a glibc
        // JVM looks — still needs the host build.
        val builds = NativePlatform.plannedBuilds(
            "linux-x86_64", "demo", "release", mapOf("linux-x86_64" to "x86_64-unknown-linux-musl"),
        )
        assertEquals(
            listOf(
                NativePlatform.NativeBuild("linux-x86_64", null, "release/libdemo.so"),
                NativePlatform.NativeBuild(
                    "linux-x86_64-musl", "x86_64-unknown-linux-musl", "x86_64-unknown-linux-musl/release/libdemo.so",
                ),
            ),
            builds,
        )
    }

    @ParameterizedTest
    @CsvSource(
        "linux-x86_64, x86_64-unknown-linux-gnu, linux-x86_64",
        "linux-x86_64, x86_64-unknown-linux-musl, linux-x86_64-musl",
        "linux-aarch64, aarch64-unknown-linux-musl, linux-aarch64-musl",
        "linux-armv7, armv7-unknown-linux-musleabihf, linux-armv7-musl",
        "linux-aarch64-musl, aarch64-unknown-linux-musl, linux-aarch64-musl",
        // An explicit -musl key with a custom target spec is kept as is.
        "linux-x86_64-musl, my-custom-target, linux-x86_64-musl",
        "macos-aarch64, aarch64-apple-darwin, macos-aarch64",
        "windows-x86_64, x86_64-pc-windows-msvc, windows-x86_64",
    )
    fun bundlesMuslTriplesUnderTheMuslClassifier(key: String, triple: String, expected: String) {
        assertEquals(expected, NativePlatform.bundlePlatform(key, triple))
    }

    @Test
    fun plansGlibcAndMuslLibrariesSideBySide() {
        val builds = NativePlatform.plannedBuilds(
            "linux-x86_64",
            "demo",
            "release",
            mapOf(
                "linux-x86_64-musl" to "x86_64-unknown-linux-musl",
                "linux-aarch64" to "aarch64-unknown-linux-musl",
            ),
        )
        assertEquals(
            listOf(
                NativePlatform.NativeBuild("linux-x86_64", null, "release/libdemo.so"),
                NativePlatform.NativeBuild(
                    "linux-aarch64-musl", "aarch64-unknown-linux-musl",
                    "aarch64-unknown-linux-musl/release/libdemo.so",
                ),
                NativePlatform.NativeBuild(
                    "linux-x86_64-musl", "x86_64-unknown-linux-musl",
                    "x86_64-unknown-linux-musl/release/libdemo.so",
                ),
            ),
            builds,
        )
    }

    @Test
    fun aMuslHostIsCoveredByAMuslTarget() {
        val viaKey = NativePlatform.plannedBuilds(
            "linux-x86_64-musl", "demo", "release", mapOf("linux-x86_64-musl" to "x86_64-unknown-linux-musl"),
        )
        assertEquals(listOf("x86_64-unknown-linux-musl"), viaKey.map { it.triple })
        val viaTriple = NativePlatform.plannedBuilds(
            "linux-x86_64-musl", "demo", "release", mapOf("linux-x86_64" to "x86_64-unknown-linux-musl"),
        )
        assertEquals(listOf("x86_64-unknown-linux-musl"), viaTriple.map { it.triple })
        // A glibc target does not cover a musl host: the host build is kept.
        val glibc = NativePlatform.plannedBuilds(
            "linux-x86_64-musl", "demo", "release", mapOf("linux-aarch64" to "aarch64-unknown-linux-gnu"),
        )
        assertEquals(
            listOf(
                NativePlatform.NativeBuild("linux-x86_64-musl", null, "release/libdemo.so"),
                NativePlatform.NativeBuild(
                    "linux-aarch64", "aarch64-unknown-linux-gnu", "aarch64-unknown-linux-gnu/release/libdemo.so",
                ),
            ),
            glibc,
        )
    }

    @Test
    fun rejectsMuslMisconfigurations() {
        assertThrows<IllegalArgumentException> {
            NativePlatform.bundlePlatform("linux-x86_64-musl", "x86_64-unknown-linux-gnu")
        }
        assertThrows<IllegalArgumentException> { NativePlatform.osOfPlatformKey("macos-aarch64-musl") }
        assertThrows<IllegalArgumentException> { NativePlatform.osOfPlatformKey("linux-x86_64-gnu") }
        val clash = assertThrows<IllegalArgumentException> {
            NativePlatform.plannedBuilds(
                "windows-x86_64", "demo", "release",
                mapOf(
                    "linux-x86_64" to "x86_64-unknown-linux-musl",
                    "linux-x86_64-musl" to "x86_64-unknown-linux-musl",
                ),
            )
        }
        assertTrue(clash.message!!.contains("native/linux-x86_64-musl/"), clash.message)
    }

    private val muslMaps = listOf(
        "55d0c0a00000-55d0c0a01000 r--p 00000000 00:2f 1234  /usr/lib/jvm/java-17-openjdk/bin/java",
        "7f1c2b000000-7f1c2b014000 r--p 00000000 00:2f 5678  /lib/ld-musl-x86_64.so.1",
        "7ffd1c000000-7ffd1c021000 rw-p 00000000 00:00 0  [stack]",
    )
    private val glibcMaps = listOf(
        "7f1c2b000000-7f1c2b028000 r--p 00000000 08:01 5678  /usr/lib/x86_64-linux-gnu/libc.so.6",
    )

    private fun dirs(vararg entries: Pair<String, List<String>>): (String) -> List<String> =
        { dir -> entries.toMap()[dir].orEmpty() }

    @Test
    fun detectsMuslFromTheMappedLibc() {
        assertTrue(NativePlatform.isMusl(null, { muslMaps }, dirs()))
        // Debian's musl package installs the loader on glibc: the mapping wins.
        assertFalse(
            NativePlatform.isMusl(null, { glibcMaps }, dirs("/lib" to listOf("ld-musl-x86_64.so.1"))),
        )
        assertTrue(NativePlatform.isMusl(null, { listOf("7f-80 r-xp 0 0 1 /lib/libc.musl-aarch64.so.1") }, dirs()))
        assertFalse(NativePlatform.isMusl(null, { listOf("7f-80 r-xp 0 0 1 /lib/libc-2.31.so") }, dirs()))
    }

    @Test
    fun fallsBackToTheLoaderFilesWithoutProc() {
        assertTrue(NativePlatform.isMusl(null, { null }, dirs("/lib" to listOf("ld-musl-aarch64.so.1"))))
        assertFalse(
            NativePlatform.isMusl(
                null,
                { null },
                dirs("/lib" to listOf("ld-musl-x86_64.so.1"), "/lib64" to listOf("ld-linux-x86-64.so.2")),
            ),
        )
        assertFalse(NativePlatform.isMusl(null, { null }, dirs()))
    }

    @ParameterizedTest
    @CsvSource("musl, true", "' MUSL ', true", "glibc, false", "gnu, false")
    fun theOverrideWinsWithoutProbing(override: String, expected: Boolean) {
        assertEquals(
            expected,
            NativePlatform.isMusl(override, { error("must not probe") }, { error("must not probe") }),
        )
    }

    @Test
    fun parsesTheMapsPathColumn() {
        assertEquals("ld-musl-x86_64.so.1", NativePlatform.mappedFileName(muslMaps[1]))
        assertEquals("libc.so.6", NativePlatform.mappedFileName("7f-80 r--p 0 08:01 5 /lib/libc.so.6 (deleted)"))
        assertNull(NativePlatform.mappedFileName(muslMaps[2]))
        assertNull(NativePlatform.mappedFileName("7f-80 rw-p 0 00:00 0 /dir/"))
    }

    @Test
    fun platformKeysCarryTheMuslClassifierOnlyOnLinux() {
        assertEquals("linux-x86_64-musl", NativePlatform.platformKey("linux", "x86_64", true))
        assertEquals("linux-x86_64", NativePlatform.platformKey("linux", "x86_64", false))
        assertEquals("macos-aarch64", NativePlatform.platformKey("macos", "aarch64", true))
        // The real host probe never throws and yields a valid key.
        NativePlatform.osOfPlatformKey(NativePlatform.hostPlatform())
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
