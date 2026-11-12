package kr.devfive.vespera

import org.gradle.api.file.DirectoryProperty
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.MapProperty
import org.gradle.api.provider.Property

/**
 * DSL for the `kr.devfive.vespera-bridge` plugin.
 *
 * ```kotlin
 * vespera {
 *     crateName.set("my_rust_lib")
 *     cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
 *     // bridgeVersion defaults to the bridge release matching this plugin
 *     autoBuildCargo.set(false) // default: opt-in
 * }
 * ```
 */
abstract class VesperaBridgeExtension {
    /**
     * Cargo crate of the Rust cdylib: either the package name
     * (`rust-jni-demo`) or its underscore library form (`rust_jni_demo`).
     * The library file name is derived Cargo-style with every `-` turned
     * into `_` (`{lib}.dll` / `lib{lib}.so` / `lib{lib}.dylib`), unless
     * [libName] is set. `cargoBuild` passes the package name to
     * `cargo build -p` — see [packageName].
     */
    abstract val crateName: Property<String>

    /**
     * Optional Cargo package name for `cargo build -p` (only used by
     * `cargoBuild`). When unset: a hyphenated [crateName] is the package
     * name; an underscore [crateName] is matched against
     * `cargo metadata --no-deps` at execution time (`rust_jni_demo` →
     * package `rust-jni-demo`), falling back to [crateName] itself.
     */
    abstract val packageName: Property<String>

    /**
     * Optional library file stem when the crate sets a custom
     * `[lib] name = "..."` in `Cargo.toml`. Defaults to
     * `crateName.replace('-', '_')`.
     */
    abstract val libName: Property<String>

    /**
     * Workspace root containing the `target/release/` directory with
     * the built cdylib.  Typically `../..` relative to a sample
     * `examples/<demo>/java/demo-app/` project layout.
     */
    abstract val cargoRoot: DirectoryProperty

    /**
     * **Deprecated, ignored.** Formerly the Rust source roots fingerprinted
     * as `cargoBuild` inputs. `cargoBuild` is never UP-TO-DATE (Cargo's own
     * incremental check decides), so fingerprinting the sources was pure
     * cost; it is no longer done. Kept so existing build scripts compile.
     */
    @Deprecated("Ignored: cargoBuild no longer fingerprints Rust sources; remove this setting.")
    abstract val cargoSourceRoots: ListProperty<String>

    /**
     * Version of `kr.devfive:vespera-bridge` to add as an
     * `implementation` dependency. Defaults to the bridge version this
     * plugin release was built with (the matching bridge release); set it
     * explicitly to pin a different one.
     */
    abstract val bridgeVersion: Property<String>

    /**
     * When `true`, registers a `cargoBuild` task that runs
     * `cargo build --release -p <package>` and wires it as a
     * dependency of `bundleNativeLib`.  Defaults to `false` (opt-in)
     * — most CI pipelines build Rust separately and don't want the
     * Java build to invoke cargo implicitly.
     */
    abstract val autoBuildCargo: Property<Boolean>

    /**
     * Cargo build profile selecting both the output subdirectory and the
     * build flag. `"release"` (default) → `cargo build --release` →
     * `target/release/`; `"dev"` (or `"debug"`) → plain `cargo build` →
     * `target/debug/`; any other `"<p>"` → `cargo build --profile <p>` →
     * `target/<p>/`. Lets debug or custom-profile cdylibs be bundled
     * without hand-editing the plugin (the previous hardcoded `--release`
     * forced every consumer onto the release profile).
     */
    abstract val cargoProfile: Property<String>

    /**
     * Base Cargo target directory that holds the per-profile output
     * subdirectory. Defaults to `<cargoRoot>/target`. Set this when the
     * workspace redirects Cargo output via `CARGO_TARGET_DIR` or a
     * `.cargo/config.toml` `build.target-dir`, so the plugin locates the
     * cdylib (and, when `autoBuildCargo` is on, directs cargo's output)
     * at the right place instead of failing on the default `target/`.
     */
    abstract val targetDir: DirectoryProperty

    /**
     * Optional directory of prebuilt libraries laid out as
     * `native/<os>-<arch>/<lib>` (for example CI artifacts from other
     * platforms). Everything under its `native/` folder is merged into the
     * jar next to the locally built libraries; a locally built library wins
     * when both provide the same file.
     */
    abstract val extraNativeLibs: DirectoryProperty

    /**
     * Extra platforms to cross-build, keyed by bridge platform
     * (`<os>-<arch>`, os one of `windows` / `macos` / `linux`) with the Rust
     * target triple as value, e.g.
     * `targets.put("linux-aarch64", "aarch64-unknown-linux-gnu")`. Each entry
     * is built with `cargo build --target <triple>` (when `autoBuildCargo`
     * is on), read from `<targetDir>/<triple>/<profile>/` and bundled under
     * `native/<os>-<arch>/` with that platform's library file name. A
     * `*-linux-musl*` triple is bundled under `native/linux-<arch>-musl/`
     * (keys may also name `linux-<arch>-musl` directly), where the bridge
     * looks first on a musl JVM. The host build is skipped when a key (or its
     * bundle folder) names the host platform. The toolchain /
     * linker for each triple must already be installed.
     */
    abstract val targets: MapProperty<String, String>
}
