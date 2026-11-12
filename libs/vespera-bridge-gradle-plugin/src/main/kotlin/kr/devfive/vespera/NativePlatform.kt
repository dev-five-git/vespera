package kr.devfive.vespera

/**
 * Pure platform / Cargo helpers behind [VesperaBridgePlugin], kept free of
 * Gradle types so they are unit-testable.
 */
internal object NativePlatform {
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
     * Every library to bundle: each `targets` entry is cross-built; the host
     * build is added unless a target already covers the host platform.
     * [libStem] is the library file stem ([libraryStem]).
     */
    fun plannedBuilds(
        hostPlatform: String,
        libStem: String,
        profile: String,
        targets: Map<String, String>,
    ): List<NativeBuild> {
        val builds = targets.toSortedMap().map { (platform, triple) ->
            require(triple.isNotBlank()) { "vespera.targets['$platform'] must name a Rust target triple." }
            NativeBuild(platform, triple, cdylibRelativePath(osOfPlatformKey(platform), libStem, profile, triple))
        }
        if (hostPlatform in targets) return builds
        val host = NativeBuild(
            hostPlatform,
            null,
            cdylibRelativePath(hostPlatform.substringBefore('-'), libStem, profile),
        )
        return listOf(host) + builds
    }
}
