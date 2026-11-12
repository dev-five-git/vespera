package kr.devfive.vespera

/**
 * Pure platform / Cargo helpers behind [VesperaBridgePlugin], kept free of
 * Gradle types so they are unit-testable.
 */
internal object NativePlatform {
    /** Directories never fingerprinted as `cargoBuild` inputs. */
    val CARGO_INPUT_EXCLUDES = listOf("**/target/**", "**/node_modules/**", "**/.git/**")

    private val PLATFORM_KEY = Regex("(windows|macos|linux)-[a-z0-9_]+")

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

    fun mapLibraryName(os: String, name: String): String = when (os) {
        "windows" -> "$name.dll"
        "macos" -> "lib$name.dylib"
        else -> "lib$name.so"
    }

    /**
     * Validate a `targets` key (`<os>-<arch>`, e.g. `linux-aarch64`) and
     * return its OS segment, which selects the library file name.
     */
    fun osOfPlatformKey(key: String): String {
        require(PLATFORM_KEY.matches(key)) {
            "vespera.targets key '$key' must look like '<os>-<arch>' with os one of " +
                "windows, macos, linux (e.g. 'linux-x86_64', 'macos-aarch64')."
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
    fun cargoCommand(crate: String, profile: String, triple: String? = null): List<String> {
        val cmd = mutableListOf("cargo", "build", "-p", crate)
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
    fun cdylibRelativePath(os: String, crate: String, profile: String, triple: String? = null): String {
        val dir = profileDir(profile)
        val lib = mapLibraryName(os, crate)
        return if (triple == null) "$dir/$lib" else "$triple/$dir/$lib"
    }

    /**
     * One native build: `platform` is the `native/<platform>/` folder,
     * `triple` the Rust target (`null` = host build).
     */
    data class NativeBuild(val platform: String, val triple: String?, val relativePath: String)

    /**
     * Every library to bundle: each `targets` entry is cross-built; the host
     * build is added unless a target already covers the host platform.
     */
    fun plannedBuilds(
        hostPlatform: String,
        crate: String,
        profile: String,
        targets: Map<String, String>,
    ): List<NativeBuild> {
        val builds = targets.toSortedMap().map { (platform, triple) ->
            require(triple.isNotBlank()) { "vespera.targets['$platform'] must name a Rust target triple." }
            NativeBuild(platform, triple, cdylibRelativePath(osOfPlatformKey(platform), crate, profile, triple))
        }
        if (hostPlatform in targets) return builds
        val host = NativeBuild(
            hostPlatform,
            null,
            cdylibRelativePath(hostPlatform.substringBefore('-'), crate, profile),
        )
        return listOf(host) + builds
    }
}
