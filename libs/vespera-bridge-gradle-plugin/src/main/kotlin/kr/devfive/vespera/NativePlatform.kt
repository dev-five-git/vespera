package kr.devfive.vespera

import java.io.IOException
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Paths

/**
 * Pure platform / Cargo helpers behind [VesperaBridgePlugin], kept free of
 * Gradle types so they are unit-testable.
 */
internal object NativePlatform {
    /** `<os>-<arch>`; only Linux takes the `-musl` libc classifier. */
    private val PLATFORM_KEY = Regex("(windows|macos)-[a-z0-9_]+|linux-[a-z0-9_]+(-musl)?")

    /** Folder classifier of a musl-libc Linux library (`native/linux-<arch>-musl/`). */
    const val MUSL_SUFFIX = "-musl"

    /** System property forcing the host libc (`musl` / `glibc` / `gnu`), as in the bridge. */
    const val LIBC_PROPERTY = "vespera.native.libc"

    /**
     * Map `os.name` to the bridge's `native/<os>-<arch>` OS segment.
     * `darwin` contains `win`, so macOS MUST be tested first (same order as
     * `VesperaNativeLoader`).
     */
    fun detectOs(osName: String): String {
        val os = osName.lowercase()
        return when {
            "mac" in os || "darwin" in os -> "macos"
            "win" in os -> "windows"
            else -> "linux"
        }
    }

    fun detectArch(osArch: String): String {
        val arch = osArch.lowercase()
        return when {
            "amd64" in arch || "x86_64" in arch -> "x86_64"
            "aarch64" in arch || "arm64" in arch -> "aarch64"
            else -> arch
        }
    }

    /**
     * Whether a Linux process runs on musl libc, with the same probe order as
     * the bridge's `NativePlatformDetector`: the [override] property, then the
     * libc actually mapped into this process (`/proc/self/maps`: musl maps
     * `ld-musl-<arch>.so.1`, glibc `libc.so.6` / `ld-linux*`). Unlike a
     * `/lib/ld-musl-*` existence check it is not fooled by Debian's `musl`
     * package on a glibc host, and unlike `ldd --version` it spawns no
     * process. Without a readable `/proc` it falls back to the loader files:
     * musl iff `/lib/ld-musl-*` exists and no `ld-linux*` does.
     * [selfMaps] returns `null` when unreadable; [listDir] an empty list.
     */
    fun isMusl(
        override: String?,
        selfMaps: () -> List<String>?,
        listDir: (String) -> List<String>,
    ): Boolean {
        when (override?.trim()?.lowercase()) {
            "musl" -> return true
            "glibc", "gnu" -> return false
        }
        selfMaps()?.forEach { line ->
            val file = mappedFileName(line) ?: return@forEach
            if (file.startsWith("ld-musl-") || file.startsWith("libc.musl-")) return true
            if (file.startsWith("libc.so.6") || file.startsWith("libc-2.") || file.startsWith("ld-linux")) {
                return false
            }
        }
        if (listDir("/lib").none { it.startsWith("ld-musl-") }) return false
        return (listDir("/lib") + listDir("/lib64")).none { it.startsWith("ld-linux") }
    }

    /** File name of the path column of a `/proc/self/maps` line, or `null`. */
    fun mappedFileName(line: String): String? {
        val slash = line.indexOf('/')
        if (slash < 0) return null
        val path = line.substring(slash).trim().removeSuffix(" (deleted)")
        return path.substringAfterLast('/').ifEmpty { null }
    }

    /** `<os>-<arch>`, plus [MUSL_SUFFIX] on a musl Linux. */
    fun platformKey(os: String, arch: String, musl: Boolean): String =
        if (os == "linux" && musl) "$os-$arch$MUSL_SUFFIX" else "$os-$arch"

    /** The platform key of the JVM running this code (the Gradle daemon = the build host). */
    fun hostPlatform(): String {
        val os = detectOs(System.getProperty("os.name", ""))
        val arch = detectArch(System.getProperty("os.arch", ""))
        val musl = os == "linux" && isMusl(System.getProperty(LIBC_PROPERTY), ::readSelfMaps, ::listDir)
        return platformKey(os, arch, musl)
    }

    private fun readSelfMaps(): List<String>? = try {
        Files.readAllLines(Paths.get("/proc/self/maps"), StandardCharsets.ISO_8859_1)
    } catch (e: IOException) {
        null
    } catch (e: SecurityException) {
        null
    }

    private fun listDir(dir: String): List<String> = try {
        Files.newDirectoryStream(Paths.get(dir)).use { entries -> entries.map { it.fileName.toString() } }
    } catch (e: IOException) {
        emptyList()
    } catch (e: SecurityException) {
        emptyList()
    }

    /** A musl Rust target triple (`x86_64-unknown-linux-musl`, `armv7-unknown-linux-musleabihf`). */
    fun isMuslTriple(triple: String): Boolean = "-linux-musl" in triple

    /**
     * The `native/<platform>/` folder a `targets` entry is bundled under: the
     * key itself, except that a plain `linux-<arch>` key built for a
     * `*-linux-musl*` triple gets the [MUSL_SUFFIX] the bridge looks for first
     * on a musl JVM (`linux-x86_64` + `x86_64-unknown-linux-musl` →
     * `linux-x86_64-musl`). A `-musl` key with a glibc triple is rejected.
     */
    fun bundlePlatform(key: String, triple: String): String {
        val os = osOfPlatformKey(key)
        val muslKey = key.endsWith(MUSL_SUFFIX)
        require(!muslKey || "-linux-gnu" !in triple) {
            "vespera.targets['$key'] names a musl platform but '$triple' is a glibc target; " +
                "use a *-linux-musl triple or the key '${key.removeSuffix(MUSL_SUFFIX)}'."
        }
        return if (os == "linux" && !muslKey && isMuslTriple(triple)) key + MUSL_SUFFIX else key
    }

    /**
     * File stem of the cdylib Cargo emits for [crateName]: an explicit
     * `[lib] name` ([libName]) wins, otherwise Cargo's default — the package
     * name with every `-` turned into `_` (`rust-jni-demo` → `rust_jni_demo`).
     */
    fun libraryStem(crateName: String, libName: String? = null): String =
        libName?.takeIf { it.isNotBlank() } ?: crateName.replace('-', '_')

    /**
     * The `cargo build -p` spec when it is known without asking Cargo: an
     * explicit [packageName], or [crateName] itself when it contains a `-`
     * (a hyphenated name can only be the package name). `null` means
     * [crateName] may be the underscore form of a hyphenated package; it is
     * resolved against `cargo metadata` at execution time
     * ([resolvePackageName]).
     */
    fun knownPackageSpec(crateName: String, packageName: String? = null): String? = when {
        !packageName.isNullOrBlank() -> packageName
        '-' in crateName -> crateName
        else -> null
    }

    private val JSON_NAME = Regex(""""name"\s*:\s*"([^"\\]+)"""")

    /**
     * Pick the Cargo package whose underscore form is [crateName] from
     * `cargo metadata --no-deps --format-version 1` output: a name `n` with
     * `n != crateName` and `n.replace('-', '_') == crateName` (so the demo's
     * `rust_jni_demo` resolves to the `rust-jni-demo` package). Falls back to
     * [crateName] when there is none (an underscore package name, or no
     * metadata). A plain scan of `"name":"..."` suffices: the only other
     * `name` equal to the underscore form is the lib target itself.
     */
    fun resolvePackageName(crateName: String, metadataJson: String?): String {
        if (metadataJson.isNullOrBlank()) return crateName
        return JSON_NAME.findAll(metadataJson)
            .map { it.groupValues[1] }
            .firstOrNull { it != crateName && it.replace('-', '_') == crateName }
            ?: crateName
    }

    fun mapLibraryName(os: String, name: String): String = when (os) {
        "windows" -> "$name.dll"
        "macos" -> "lib$name.dylib"
        else -> "lib$name.so"
    }

    /**
     * Validate a `targets` key (`<os>-<arch>`, e.g. `linux-aarch64`, or
     * `linux-<arch>-musl`) and return its OS segment, which selects the
     * library file name.
     */
    fun osOfPlatformKey(key: String): String {
        require(PLATFORM_KEY.matches(key)) {
            "vespera.targets key '$key' must look like '<os>-<arch>' with os one of " +
                "windows, macos, linux (e.g. 'linux-x86_64', 'macos-aarch64'), or 'linux-<arch>-musl'."
        }
        return key.substringBefore('-')
    }

    /**
     * Map a Cargo profile name to its `target/` output subdirectory.
     * Cargo's built-in `dev` profile emits to `debug`; every other profile
     * (`release`, or a custom `[profile.X]`) uses its own name verbatim.
     */
    fun profileDir(profile: String): String = when (profile) {
        "dev", "debug" -> "debug"
        else -> profile
    }

    /** Profile-aware `cargo build` command, optionally cross-compiling. */
    fun cargoCommand(packageSpec: String, profile: String, triple: String? = null): List<String> {
        val cmd = mutableListOf("cargo", "build", "-p", packageSpec)
        when (profile) {
            "release" -> cmd.add("--release")
            "dev", "debug" -> {} // default profile → target/debug
            else -> { cmd.add("--profile"); cmd.add(profile) }
        }
        if (triple != null) {
            cmd.add("--target")
            cmd.add(triple)
        }
        return cmd
    }

    /**
     * Path of a built cdylib relative to the Cargo target directory: cross
     * builds land under `<triple>/<profileDir>/`, host builds directly under
     * `<profileDir>/`.
     */
    fun cdylibRelativePath(os: String, libStem: String, profile: String, triple: String? = null): String {
        val dir = profileDir(profile)
        val lib = mapLibraryName(os, libStem)
        return if (triple == null) "$dir/$lib" else "$triple/$dir/$lib"
    }

    /**
     * One native build: `platform` is the `native/<platform>/` folder,
     * `triple` the Rust target (`null` = host build).
     */
    data class NativeBuild(val platform: String, val triple: String?, val relativePath: String)

    /**
     * Every library to bundle: each `targets` entry is cross-built into its
     * [bundlePlatform] folder; the host build is added unless a target already
     * covers the host platform (its key or its bundle folder names it).
     * [hostPlatform] carries the `-musl` classifier on a musl build host.
     * [libStem] is the library file stem ([libraryStem]).
     */
    fun plannedBuilds(
        hostPlatform: String,
        libStem: String,
        profile: String,
        targets: Map<String, String>,
    ): List<NativeBuild> {
        val builds = targets.toSortedMap().map { (key, triple) ->
            require(triple.isNotBlank()) { "vespera.targets['$key'] must name a Rust target triple." }
            NativeBuild(
                bundlePlatform(key, triple),
                triple,
                cdylibRelativePath(osOfPlatformKey(key), libStem, profile, triple),
            )
        }
        builds.groupBy { it.platform }.forEach { (platform, same) ->
            require(same.size == 1) {
                "vespera.targets bundle more than one library into native/$platform/: " +
                    same.joinToString { it.triple.orEmpty() }
            }
        }
        if (hostPlatform in targets || builds.any { it.platform == hostPlatform }) return builds
        val host = NativeBuild(
            hostPlatform,
            null,
            cdylibRelativePath(hostPlatform.substringBefore('-'), libStem, profile),
        )
        return listOf(host) + builds
    }
}
