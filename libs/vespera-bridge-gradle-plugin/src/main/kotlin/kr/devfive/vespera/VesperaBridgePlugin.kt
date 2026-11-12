package kr.devfive.vespera

import org.gradle.api.file.DuplicatesStrategy
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.Task
import org.gradle.api.tasks.Copy
import org.gradle.language.jvm.tasks.ProcessResources
import java.io.File

/**
 * Gradle plugin that wires a Vespera Rust cdylib into a Java
 * application:
 *
 * 1. Registers a `bundleNativeLib` task that copies the host cdylib (and every
 *    `vespera.targets` cross-build) from the Cargo target directory into a
 *    generated resources directory, merged with `vespera.extraNativeLibs`.
 * 2. Wires those generated resources into `processResources` under
 *    `native/<os>-<arch>/` so `VesperaBridge.init(...)` can extract it.
 * 3. Adds `kr.devfive:vespera-bridge:<bridgeVersion>` as an
 *    `implementation` dependency.
 * 4. Optionally (`autoBuildCargo = true`) registers a `cargoBuild`
 *    task that invokes `cargo build -p <crateName>` (plus one
 *    `--target <triple>` build per `vespera.targets` entry) before
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
        ext.targets.convention(emptyMap())

        // Compute platform-derived values eagerly (host machine info).
        val hostPlatform = NativePlatform.detectOs(System.getProperty("os.name", "")) +
            "-" + NativePlatform.detectArch(System.getProperty("os.arch", ""))
        val generatedResourcesDir = project.layout.buildDirectory.dir("generated/vesperaNativeResources")

        // Lazy references, evaluated when the tasks are realised. Every
        // cdylib lives under `<targetDir|cargoRoot/target>/[<triple>/]<profileDir>/`.
        val targetBase = project.provider {
            if (ext.targetDir.isPresent) ext.targetDir.get().asFile
            else File(ext.cargoRoot.get().asFile, "target")
        }
        val plannedBuilds = project.provider {
            NativePlatform.plannedBuilds(
                hostPlatform,
                ext.crateName.get(),
                ext.cargoProfile.get(),
                ext.targets.get(),
            )
        }

        val cargoBuildTask = project.tasks.register(
            "cargoBuild",
            CargoBuildTask::class.java,
            object : org.gradle.api.Action<CargoBuildTask> {
                override fun execute(t: CargoBuildTask) {
                    t.group = "vespera"
                    t.description = "Build the Rust cdylib (host + vespera.targets) via `cargo build`."
                    t.workingDirectory.set(ext.cargoRoot)
                    t.commands.set(plannedBuilds.map { builds ->
                        builds.map { b ->
                            NativePlatform.cargoCommand(ext.crateName.get(), ext.cargoProfile.get(), b.triple)
                        }
                    })
                    // Honour a redirected target dir so cargo writes where
                    // `bundleNativeLib` later looks for the cdylib.
                    t.cargoTargetDir.set(ext.targetDir.map { it.asFile.absolutePath })
                    // Inputs: manifests, Cargo.lock and Rust sources in the
                    // configured roots, never build output, node_modules or
                    // .git (fingerprinting those is slow and meaningless).
                    // The task is never UP-TO-DATE anyway (see CargoBuildTask).
                    val cargoRoot = ext.cargoRoot.get().asFile
                    val cargoInputs = project.fileTree(cargoRoot)
                    cargoInputs.include("Cargo.toml")
                    cargoInputs.include("**/Cargo.toml")
                    ext.cargoSourceRoots.get().forEach { root ->
                        cargoInputs.include("${root.trimEnd('/', '\\')}/**/*.rs")
                    }
                    cargoInputs.exclude(NativePlatform.CARGO_INPUT_EXCLUDES)
                    // Cargo.lock goes through the tree: a missing lock file
                    // (fresh library crate) must not fail input validation,
                    // which `inputs.file(...).optional()` still does.
                    cargoInputs.include("Cargo.lock")
                    t.inputs.files(cargoInputs)
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
                        "Copy the built cdylib(s) into generated resources/native/<os>-<arch>/."
                    t.into(generatedResourcesDir)
                    // Locally built libraries win over extraNativeLibs.
                    t.duplicatesStrategy = DuplicatesStrategy.EXCLUDE
                    val builds = plannedBuilds.get()
                    builds.forEach { build ->
                        t.from(targetBase.map { File(it, build.relativePath) }) {
                            into("native/${build.platform}")
                        }
                    }
                    if (ext.extraNativeLibs.isPresent) {
                        t.from(ext.extraNativeLibs) { include("native/**") }
                    }
                    t.doFirst(object : org.gradle.api.Action<Task> {
                        override fun execute(@Suppress("UNUSED_PARAMETER") task: Task) {
                            builds.forEach { build ->
                                val src = File(targetBase.get(), build.relativePath)
                                require(src.exists()) {
                                    "Native library not found: $src\n" +
                                        "Build the '${ext.crateName.get()}' cdylib for the " +
                                        "'${ext.cargoProfile.get()}' profile" +
                                        (build.triple?.let { " and target '$it'" } ?: "") +
                                        " (or set vespera.autoBuildCargo = true). If the " +
                                        "workspace redirects Cargo output (CARGO_TARGET_DIR / " +
                                        ".cargo/config.toml build.target-dir), set " +
                                        "vespera.targetDir to that directory."
                                }
                            }
                        }
                    })
                }
            }
        )

        // Wire cargoBuild -> bundleNativeLib when opt-in.
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
            // Management): Gradle's "fail-on-project-repos" mode
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
}
