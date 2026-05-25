package kr.devfive.vespera

import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.Task
import org.gradle.api.tasks.Copy
import org.gradle.api.tasks.Exec
import java.io.File

/**
 * Gradle plugin that wires a Vespera Rust cdylib into a Java
 * application:
 *
 * 1. Registers a `bundleNativeLib` task that copies the cdylib from
 *    `<cargoRoot>/target/release/` into
 *    `build/resources/main/native/<os>-<arch>/` so
 *    `VesperaBridge.init(...)` can extract it at runtime.
 * 2. Wires `bundleNativeLib` into `processResources`.
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
 *     id("kr.devfive.vespera-bridge") version "0.0.15"
 * }
 *
 * vespera {
 *     crateName.set("my_rust_lib")
 *     cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
 *     bridgeVersion.set("0.0.15")
 * }
 * ```
 */
class VesperaBridgePlugin : Plugin<Project> {
    override fun apply(project: Project) {
        val ext = project.extensions
            .create("vespera", VesperaBridgeExtension::class.java)
        ext.autoBuildCargo.convention(false)

        // Compute platform-derived values eagerly (host machine info).
        val os = detectOs()
        val arch = detectArch()
        val targetSubdir = "resources/main/native/$os-$arch"

        // Lazy file references — evaluated at task execution.
        val cdylibFile = project.provider {
            val root = ext.cargoRoot.get().asFile
            val name = ext.crateName.get()
            File(root, "target/release/" + mapLibraryName(os, name))
        }

        val cargoBuildTask = project.tasks.register(
            "cargoBuild",
            Exec::class.java,
            object : org.gradle.api.Action<Exec> {
                override fun execute(t: Exec) {
                    t.group = "vespera"
                    t.description = "Build the Rust cdylib via `cargo build --release`."
                    t.workingDir = ext.cargoRoot.get().asFile
                    t.commandLine("cargo", "build", "-p", ext.crateName.get(), "--release")
                    // Up-to-date check: re-run on any .rs file or Cargo.lock change.
                    val rustSources = project.fileTree(
                        ext.cargoRoot.get().asFile.resolve("src")
                    )
                    rustSources.include("**/*.rs")
                    t.inputs.files(rustSources)
                    t.inputs.file(ext.cargoRoot.get().asFile.resolve("Cargo.lock"))
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
                        "Copy the built cdylib into src/main/resources/native/<os>-<arch>/."
                    t.from(cdylibFile)
                    t.into(project.layout.buildDirectory.dir(targetSubdir))
                    t.doFirst(object : org.gradle.api.Action<Task> {
                        override fun execute(@Suppress("UNUSED_PARAMETER") task: Task) {
                            val src = cdylibFile.get()
                            require(src.exists()) {
                                "Native library not found: $src\n" +
                                    "Run: cargo build -p ${ext.crateName.get()} --release " +
                                    "(or set vespera.autoBuildCargo = true)"
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

        // Hook into Java resource processing + dependency wiring.
        project.afterEvaluate(object : org.gradle.api.Action<Project> {
            override fun execute(p: Project) {
                p.tasks.findByName("processResources")?.dependsOn(bundleTask)

                // Repository configuration is intentionally left to
                // the user's settings.gradle.kts (dependencyResolution
                // Management) — Gradle's "fail-on-project-repos" mode
                // requires us not to mutate project.repositories from
                // a plugin.  Users typically add mavenCentral() (and
                // mavenLocal() for development) at the settings level.
                val version = ext.bridgeVersion.orNull
                    ?: error(
                        "vespera.bridgeVersion must be set explicitly. " +
                            "Example: vespera { bridgeVersion.set(\"0.0.15\") }"
                    )
                p.dependencies.add(
                    "implementation",
                    "kr.devfive:vespera-bridge:$version",
                )
            }
        })
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
}
