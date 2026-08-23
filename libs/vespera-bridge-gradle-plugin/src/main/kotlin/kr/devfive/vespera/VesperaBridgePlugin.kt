package kr.devfive.vespera

import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.Task
import org.gradle.api.tasks.Copy
import org.gradle.api.tasks.Exec
import org.gradle.language.jvm.tasks.ProcessResources
import java.io.File

/**
 * Gradle plugin that wires a Vespera Rust cdylib into a Java
 * application:
 *
 * 1. Registers a `bundleNativeLib` task that copies the cdylib from
 *    `<cargoRoot>/target/release/` into a generated resources directory.
 * 2. Wires those generated resources into `processResources` under
 *    `native/<os>-<arch>/` so `VesperaBridge.init(...)` can extract it.
 * 3. Adds `kr.devfive:vespera-bridge:<bridgeVersion>` as an
 *    `implementation` dependency.
 * 4. Optionally (`autoBuildCargo = true`) registers a `cargoBuild`
 *    task that invokes `cargo build --release -p <crateName>` before
 *    `bundleNativeLib`.
 *
 * Usage:
 *
 * ```kotlin
 * plugins {
 *     id("kr.devfive.vespera-bridge") version "<plugin-version>"
 * }
 *
 * vespera {
 *     crateName.set("my_rust_lib")
 *     cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
 *     bridgeVersion.set("<bridge-version>")
 * }
 * ```
 */
class VesperaBridgePlugin : Plugin<Project> {
    override fun apply(project: Project) {
        val ext = project.extensions
            .create("vespera", VesperaBridgeExtension::class.java)
        ext.autoBuildCargo.convention(false)
        ext.cargoSourceRoots.convention(listOf("src", "crates", "examples"))
        ext.cargoProfile.convention("release")

        // Compute platform-derived values eagerly (host machine info).
        val os = detectOs()
        val arch = detectArch()
        val generatedResourcesDir = project.layout.buildDirectory.dir("generated/vesperaNativeResources")
        val targetSubdir = "native/$os-$arch"

        // Lazy file references — evaluated at task execution.  The cdylib
        // lives under `<targetDir|cargoRoot/target>/<profileDir>/`, so a
        // debug / custom-profile build or a redirected CARGO_TARGET_DIR is
        // located correctly instead of being hardcoded to `target/release/`.
        val cdylibFile = project.provider {
            val name = ext.crateName.get()
            val targetBase =
                if (ext.targetDir.isPresent) ext.targetDir.get().asFile
                else File(ext.cargoRoot.get().asFile, "target")
            File(targetBase, profileDir(ext.cargoProfile.get()) + "/" + mapLibraryName(os, name))
        }

        val cargoBuildTask = project.tasks.register(
            "cargoBuild",
            Exec::class.java,
            object : org.gradle.api.Action<Exec> {
                override fun execute(t: Exec) {
                    t.group = "vespera"
                    t.description = "Build the Rust cdylib via `cargo build --release`."
                    t.workingDir = ext.cargoRoot.get().asFile
                    // Profile-aware command: `release` → `--release`, `dev`/
                    // `debug` → default build, any other → `--profile <p>`.
                    val profile = ext.cargoProfile.get()
                    val cmd = mutableListOf("cargo", "build", "-p", ext.crateName.get())
                    when (profile) {
                        "release" -> cmd.add("--release")
                        "dev", "debug" -> {} // default profile → target/debug
                        else -> { cmd.add("--profile"); cmd.add(profile) }
                    }
                    t.commandLine(cmd)
                    // Honour a redirected target dir so cargo writes where
                    // `bundleNativeLib` later looks for the cdylib.
                    if (ext.targetDir.isPresent) {
                        t.environment(
                            "CARGO_TARGET_DIR",
                            ext.targetDir.get().asFile.absolutePath,
                        )
                    }
                    // Up-to-date check: re-run on workspace manifests, Cargo.lock,
                    // and Rust sources in configured roots. This repository keeps
                    // Rust code under crates/* and examples/*, not only src/.
                    val cargoRoot = ext.cargoRoot.get().asFile
                    val cargoInputs = project.fileTree(cargoRoot)
                    cargoInputs.include("Cargo.toml")
                    cargoInputs.include("**/Cargo.toml")
                    ext.cargoSourceRoots.get().forEach { root ->
                        cargoInputs.include("${root.trimEnd('/', '\\')}/**/*.rs")
                    }
                    t.inputs.files(cargoInputs)
                    t.inputs.file(cargoRoot.resolve("Cargo.lock")).optional()
                    t.outputs.file(cdylibFile)
                }
            }
        )

        val bundleTask = project.tasks.register(
            "bundleNativeLib",
            Copy::class.java,
            object : org.gradle.api.Action<Copy> {
                override fun execute(t: Copy) {
                    t.group = "vespera"
                    t.description =
                        "Copy the built cdylib into generated resources/native/<os>-<arch>/."
                    t.from(cdylibFile)
                    t.into(generatedResourcesDir.map { it.dir(targetSubdir) })
                    t.doFirst(object : org.gradle.api.Action<Task> {
                        override fun execute(@Suppress("UNUSED_PARAMETER") task: Task) {
                            val src = cdylibFile.get()
                            require(src.exists()) {
                                "Native library not found: $src\n" +
                                    "Build the '${ext.crateName.get()}' cdylib for the " +
                                    "'${ext.cargoProfile.get()}' profile (or set " +
                                    "vespera.autoBuildCargo = true). If the workspace " +
                                    "redirects Cargo output (CARGO_TARGET_DIR / " +
                                    ".cargo/config.toml build.target-dir), set " +
                                    "vespera.targetDir to that directory."
                            }
                        }
                    })
                }
            }
        )

        // Wire cargoBuild → bundleNativeLib when opt-in.
        bundleTask.configure(object : org.gradle.api.Action<Copy> {
            override fun execute(t: Copy) {
                t.dependsOn(
                    project.provider {
                        if (ext.autoBuildCargo.get()) listOf(cargoBuildTask) else emptyList<Any>()
                    }
                )
            }
        })

        // Hook into Java resource processing + dependency wiring lazily when a
        // Java plugin creates `processResources` / `implementation`.  Avoid
        // afterEvaluate so configuration-cache snapshots do not depend on a
        // late mutable project callback.
        project.pluginManager.withPlugin("java") {
            project.tasks.withType(ProcessResources::class.java).configureEach {
                dependsOn(bundleTask)
                from(generatedResourcesDir)
            }

            // Repository configuration is intentionally left to
            // the user's settings.gradle.kts (dependencyResolution
            // Management) — Gradle's "fail-on-project-repos" mode
            // requires us not to mutate project.repositories from
            // a plugin.  Users typically add mavenCentral() (and
            // mavenLocal() for development) at the settings level.
            val bridgeDependency = ext.bridgeVersion
                .map { version -> "kr.devfive:vespera-bridge:$version" }
                .orElse(project.provider {
                    error(
                        "vespera.bridgeVersion must be set explicitly. " +
                            "Example: vespera { bridgeVersion.set(\"<bridge-version>\") }"
                    )
                })
            project.dependencies.addProvider("implementation", bridgeDependency)
        }
    }

    private fun detectOs(): String {
        val os = System.getProperty("os.name", "").lowercase()
        return when {
            "win" in os -> "windows"
            "mac" in os || "darwin" in os -> "macos"
            else -> "linux"
        }
    }

    private fun detectArch(): String {
        val arch = System.getProperty("os.arch", "").lowercase()
        return when {
            "amd64" in arch || "x86_64" in arch -> "x86_64"
            "aarch64" in arch || "arm64" in arch -> "aarch64"
            else -> arch
        }
    }

    private fun mapLibraryName(os: String, name: String): String = when (os) {
        "windows" -> "$name.dll"
        "macos" -> "lib$name.dylib"
        else -> "lib$name.so"
    }

    /**
     * Map a Cargo profile name to its `target/` output subdirectory.
     * Cargo's built-in `dev` profile emits to `debug`; every other profile
     * (`release`, or a custom `[profile.X]`) uses its own name verbatim.
     */
    private fun profileDir(profile: String): String = when (profile) {
        "dev", "debug" -> "debug"
        else -> profile
    }
}
