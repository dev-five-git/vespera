package kr.devfive.vespera

import org.gradle.api.file.DirectoryProperty
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.Property

/**
 * DSL for the `kr.devfive.vespera-bridge` plugin.
 *
 * ```kotlin
 * vespera {
 *     crateName.set("my_rust_lib")
     *     cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
     *     cargoSourceRoots.add("apps/native")
     *     bridgeVersion.set("<bridge-version>")
     *     autoBuildCargo.set(false) // default: opt-in
     * }
 * ```
 */
abstract class VesperaBridgeExtension {
    /**
     * Cargo crate name of the Rust cdylib — used to derive the
     * platform-specific library filename
     * (`{name}.dll` / `lib{name}.so` / `lib{name}.dylib`).
     */
    abstract val crateName: Property<String>

    /**
     * Workspace root containing the `target/release/` directory with
     * the built cdylib.  Typically `../..` relative to a sample
     * `examples/<demo>/java/demo-app/` project layout.
     */
    abstract val cargoRoot: DirectoryProperty

    /**
     * Cargo source roots, relative to {@link #cargoRoot}, watched by the
     * optional {@code cargoBuild} task. Each root contributes
     * {@code <root>/**/*.rs}; the plugin also always watches every
     * {@code Cargo.toml} and {@code Cargo.lock}. Defaults cover a single
     * crate ({@code src}) plus this repository's workspace layout
     * ({@code crates}, {@code examples}).
     */
    abstract val cargoSourceRoots: ListProperty<String>

    /**
     * Version of `kr.devfive:vespera-bridge` to add as an
     * `implementation` dependency.  Must be set explicitly — the
     * plugin does not guess a default to avoid silent upgrades.
     */
    abstract val bridgeVersion: Property<String>

    /**
     * When `true`, registers a `cargoBuild` task that runs
     * `cargo build --release -p <crateName>` and wires it as a
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
}
