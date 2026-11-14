package kr.devfive.vespera

import org.gradle.api.file.DuplicatesStrategy
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.Task
import org.gradle.api.provider.ValueSource
import org.gradle.api.provider.ValueSourceParameters
import org.gradle.api.provider.ValueSourceSpec
import org.gradle.api.tasks.Sync
import org.gradle.language.jvm.tasks.ProcessResources
import java.io.File
import java.util.Properties

/**
 * Gradle plugin that wires a Vespera Rust cdylib into a Java
 * application:
 *
 * 1. Registers a `bundleNativeLib` `Sync` task that copies the host cdylib (and every
 *    `vespera.targets` cross-build) from the Cargo target directory into a
 *    generated resources directory, merged with `vespera.extraNativeLibs`
 *    (a plugin-owned directory: a dropped target or renamed crate leaves no
 *    stale library behind).
 * 2. Wires those generated resources into `processResources` under
 *    `native/<os>-<arch>/` so `VesperaBridge.init(...)` can extract it.
 * 3. Adds `kr.devfive:vespera-bridge:<bridgeVersion>` as an
 *    `implementation` dependency (default: the bridge release this plugin
 *    was built with).
 * 4. Optionally (`autoBuildCargo = true`) registers a `cargoBuild`
 *    task that invokes `cargo build -p <package>` (plus one
 *    `--target <triple>` build per `vespera.targets` entry) before
 *    `bundleNativeLib`.
 * 5. Packages `vespera.nativeJarPlatforms` as per-platform classifier jars
 *    (`nativeJar<Platform>` / `nativeJars`, see [NativeJars]) and, with
 *    `maven-publish`, publishes them as the `vesperaNative` publication.
 *
 * Usage:
 *
 * ```kotlin
 * plugins {
 *     id("kr.devfive.vespera-bridge") version "<plugin-version>"
 * }
 *
 * vespera {
 *     crateName.set("my-rust-lib") // or "my_rust_lib"
 *     cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
 * }
 * ```
 */
class VesperaBridgePlugin : Plugin<Project> {
    override fun apply(project: Project) {
        val ext = project.extensions
            .create("vespera", VesperaBridgeExtension::class.java)
        ext.autoBuildCargo.convention(false)
        @Suppress("DEPRECATION")
        ext.cargoSourceRoots.convention(listOf("src", "crates", "examples"))
        ext.cargoProfile.convention("release")
        ext.targets.convention(emptyMap())
        ext.bundleNative.convention(true)
        NativeJars.register(project, ext)
        embeddedBridgeVersion()?.let { ext.bridgeVersion.convention(it) }

        // Host platform (`<os>-<arch>`, `-musl` on a musl Linux host). Probed in
        // a ValueSource: its result (not `/proc/self/maps`, which differs per
        // process) is the configuration-cache fingerprint.
        val hostPlatform = project.providers.of(
            HostPlatformValueSource::class.java,
            object : org.gradle.api.Action<ValueSourceSpec<ValueSourceParameters.None>> {
                override fun execute(spec: ValueSourceSpec<ValueSourceParameters.None>) {}
            },
        )
        val generatedResourcesDir = project.layout.buildDirectory.dir("generated/vesperaNativeResources")

        // Lazy references, evaluated when the tasks are realised. Every
        // cdylib lives under `<targetDir|cargoRoot/target>/[<triple>/]<profileDir>/`.
        val targetBase = ext.targetDir.map { it.asFile }
            .orElse(ext.cargoRoot.map { File(it.asFile, "target") })
        val plannedBuilds = ext.crateName.zip(ext.cargoProfile) { crate, profile ->
            NativePlatform.plannedBuilds(
                hostPlatform.get(),
                NativePlatform.libraryStem(crate, ext.libName.orNull),
                profile,
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
                    t.crateName.set(ext.crateName)
                    t.packageName.set(ext.packageName)
                    t.profile.set(ext.cargoProfile)
                    t.triples.set(plannedBuilds.map { builds -> builds.map { it.triple ?: "" } })
                    // Honour a redirected target dir so cargo writes where
                    // `bundleNativeLib` later looks for the cdylib.
                    t.cargoTargetDir.set(ext.targetDir.map { it.asFile.absolutePath })
                    // No file inputs: the task is never UP-TO-DATE (see
                    // CargoBuildTask), so fingerprinting every Rust source on
                    // each run would be pure cost.
                }
            }
        )

        val bundleTask = project.tasks.register(
            "bundleNativeLib",
            Sync::class.java,
            object : org.gradle.api.Action<Sync> {
                override fun execute(t: Sync) {
                    t.group = "vespera"
                    t.description =
                        "Sync the built cdylib(s) into generated resources/native/<os>-<arch>/ (stale libraries removed)."
                    t.into(generatedResourcesDir)
                    // Locally built libraries win over extraNativeLibs.
                    t.duplicatesStrategy = DuplicatesStrategy.EXCLUDE
                    // Resolved during task configuration so the doFirst action
                    // below captures only plain values (no extension / project
                    // reference at execution time — configuration-cache safe).
                    val base = targetBase.get()
                    val crate = ext.crateName.get()
                    val profile = ext.cargoProfile.get()
                    val expected = plannedBuilds.get().map { build ->
                        val src = File(base, build.relativePath)
                        t.from(src) { into("native/${build.platform}") }
                        ExpectedLibrary(src, build.triple)
                    }
                    if (ext.extraNativeLibs.isPresent) {
                        t.from(ext.extraNativeLibs) { include("native/**") }
                    }
                    t.doFirst(object : org.gradle.api.Action<Task> {
                        override fun execute(@Suppress("UNUSED_PARAMETER") task: Task) {
                            expected.forEach { lib -> lib.requireExists(crate, profile) }
                        }
                    })
                }
            }
        )

        // Wire cargoBuild -> bundleNativeLib when opt-in.
        bundleTask.configure(object : org.gradle.api.Action<Sync> {
            override fun execute(t: Sync) {
                t.dependsOn(
                    ext.autoBuildCargo.map { auto ->
                        if (auto) listOf(cargoBuildTask) else emptyList<Any>()
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
                // `bundleNative = false` (the app loads its library from
                // per-platform native jars) leaves bundleNativeLib unrealised.
                dependsOn(ext.bundleNative.map { if (it) listOf(bundleTask) else emptyList<Any>() })
                from(ext.bundleNative.map { if (it) listOf(generatedResourcesDir) else emptyList<Any>() })
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
                        "vespera.bridgeVersion is not set and this plugin build embeds no " +
                            "default. Example: vespera { bridgeVersion.set(\"<bridge-version>\") }"
                    )
                })
            project.dependencies.addProvider("implementation", bridgeDependency)
        }
    }

    /** One library `bundleNativeLib` expects; plain data, configuration-cache serializable. */
    private data class ExpectedLibrary(val file: File, val triple: String?) : java.io.Serializable {
        fun requireExists(crate: String, profile: String) {
            require(file.exists()) {
                "Native library not found: $file\n" +
                    "Build the '$crate' cdylib for the '$profile' profile" +
                    (triple?.let { " and target '$it'" } ?: "") +
                    " (or set vespera.autoBuildCargo = true). If the " +
                    "workspace redirects Cargo output (CARGO_TARGET_DIR / " +
                    ".cargo/config.toml build.target-dir), set " +
                    "vespera.targetDir to that directory."
            }
        }
    }

    /** The build host's bridge platform key ([NativePlatform.hostPlatform]). */
    internal abstract class HostPlatformValueSource : ValueSource<String, ValueSourceParameters.None> {
        override fun obtain(): String = NativePlatform.hostPlatform()
    }

    internal companion object {
        /** Classpath resource (next to this class) holding the bridge version this plugin was built with. */
        const val VERSION_RESOURCE = "vespera-bridge-plugin.properties"

        /** The embedded default `bridgeVersion`, or `null` when the resource is missing. */
        fun embeddedBridgeVersion(): String? =
            VesperaBridgePlugin::class.java.getResourceAsStream(VERSION_RESOURCE)?.use { input ->
                Properties().apply { load(input) }.getProperty("bridgeVersion")
                    ?.trim()?.takeIf { it.isNotEmpty() }
            }
    }
}
