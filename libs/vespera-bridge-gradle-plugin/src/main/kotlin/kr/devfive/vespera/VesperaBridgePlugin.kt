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
 *    stale library behind). It depends on `verifyNativeLibs`, which fails the
 *    build when an expected cdylib was not built — also when none was, where
 *    the `Sync` alone would be skipped as NO-SOURCE.
 * 2. Wires those generated resources into `processResources` under
 *    `native/<os>-<arch>/` so `VesperaBridge.init(...)` can extract it.
 * 3. Adds `kr.devfive:vespera-bridge:<bridgeVersion>` as an
 *    `implementation` dependency (default: the bridge release this plugin
 *    was built with).
 * 4. Registers a `cargoBuild` task that invokes `cargo build -p <package>`
 *    (plus one `--target <triple>` build per `vespera.targets` entry). It
 *    runs before `bundleNativeLib` only with `autoBuildCargo = true`; by
 *    default it is run explicitly or not at all.
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

        // A separate task, not a doFirst on the Sync: with no library built at
        // all (and no extraNativeLibs) every Sync source is missing, Gradle
        // skips the Sync as NO-SOURCE — its actions never run — and the jar
        // would silently ship without a native library.
        val verifyTask = project.tasks.register(
            "verifyNativeLibs",
            object : org.gradle.api.Action<Task> {
                override fun execute(t: Task) {
                    t.group = "vespera"
                    t.description = "Fail when a cdylib bundleNativeLib should bundle was not built."
                    // The action captures providers only (no extension /
                    // project reference — configuration-cache safe), read when
                    // the task runs: a build script that realises this task
                    // before its `vespera {}` block (an eager
                    // `tasks.withType<...> {}`) still sees the final values. A
                    // native-jar consumer (`crateName` + `bundleNative = false`,
                    // no cargoRoot / targetDir) realises it on `tasks --all` /
                    // IDE sync too, so a missing location only fails when the
                    // task runs.
                    val baseP = targetBase
                    val crateP = ext.crateName
                    val profileP = ext.cargoProfile
                    val extraP = ext.extraNativeLibs
                    val expectedP = expectedLibraries(targetBase, plannedBuilds)
                    t.doLast(object : org.gradle.api.Action<Task> {
                        override fun execute(@Suppress("UNUSED_PARAMETER") task: Task) {
                            val base = baseP.orNull
                            val crate = crateP.orNull
                            val profile = profileP.get()
                            // Prebuilt-only jar: extraNativeLibs without cargoRoot /
                            // targetDir bundles just those libraries, no local build.
                            val prebuiltDir = if (base == null) extraP.orNull?.asFile else null
                            val expected = expectedP.get()
                            if (prebuiltDir != null) {
                                requirePrebuilt(prebuiltDir)
                                return
                            }
                            requireLocation(crate, base)
                            expected.forEach { lib -> lib.requireExists(crate.orEmpty(), profile) }
                        }
                    })
                    // Wire cargoBuild -> verifyNativeLibs -> bundleNativeLib when opt-in.
                    t.dependsOn(
                        ext.autoBuildCargo.map { auto ->
                            if (auto) listOf(cargoBuildTask) else emptyList<Any>()
                        }
                    )
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
                    t.dependsOn(verifyTask)
                    t.into(generatedResourcesDir)
                    // Locally built libraries win over extraNativeLibs.
                    t.duplicatesStrategy = DuplicatesStrategy.EXCLUDE
                    // Lazy sources, resolved when the task runs (an eager
                    // `tasks.withType<Sync> {}` above the `vespera {}` block
                    // realises this task before cargoRoot / crateName are
                    // set). Without a location there is nothing to copy, and
                    // verifyNativeLibs (a dependency) fails with the reason
                    // before this runs.
                    val planned = targetBase.zip(plannedBuilds) { base, builds ->
                        builds.associate { build -> pathKey(File(base, build.relativePath)) to build.platform }
                    }.orElse(emptyMap())
                    // The folder each library is copied to is decided in the
                    // eachFile closure, which Gradle does not track: a changed
                    // mapping (glibc <-> musl on the host, a re-keyed target)
                    // must re-run the task, so it is declared as an input.
                    t.inputs.property(
                        "bundledFolders",
                        // Prebuilt-only setups (extraNativeLibs, no crateName) plan no
                        // build: an empty value, not a missing one, fails no task.
                        plannedBuilds.map { builds ->
                            builds.joinToString("\n") { "${it.relativePath}=${it.platform}" }
                        }.orElse(""),
                    )
                    t.from(planned.map { files -> files.keys.map(::File) }) {
                        eachFile(object : org.gradle.api.Action<org.gradle.api.file.FileCopyDetails> {
                            override fun execute(details: org.gradle.api.file.FileCopyDetails) {
                                val platform = planned.get().getValue(pathKey(details.file))
                                details.path = "native/$platform/${details.name}"
                            }
                        })
                    }
                    t.from(ext.extraNativeLibs.map { listOf(it) }.orElse(emptyList())) { include("native/**") }
                }
            }
        )

        // Hook into Java resource processing + dependency wiring lazily when a
        // Java plugin creates `processResources` / `implementation`.  Avoid
        // afterEvaluate so configuration-cache snapshots do not depend on a
        // late mutable project callback.
        project.pluginManager.withPlugin("java") {
            // Only the main source set's resources: test (and other source
            // sets') resources would copy every cdylib again and depend on
            // cargoBuild for nothing.
            project.tasks.named(
                org.gradle.api.plugins.JavaPlugin.PROCESS_RESOURCES_TASK_NAME,
                ProcessResources::class.java,
            ).configure {
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

        /** Normalised absolute path: matches a planned library to the file Gradle copies. */
        fun pathKey(file: File): String = file.absoluteFile.normalize().path

        /** Every library `bundleNativeLib` expects (empty without a location), evaluated lazily. */
        private fun expectedLibraries(
            targetBase: org.gradle.api.provider.Provider<File>,
            plannedBuilds: org.gradle.api.provider.Provider<List<NativePlatform.NativeBuild>>,
        ): org.gradle.api.provider.Provider<List<ExpectedLibrary>> =
            targetBase.zip(plannedBuilds) { base, builds ->
                builds.map { build -> ExpectedLibrary(File(base, build.relativePath), build.triple) }
            }.orElse(emptyList())

        /**
         * Fail `verifyNativeLibs` when the cdylib cannot be located at all.
         * Static (companion) so the task action captures no plugin instance.
         */
        fun requireLocation(crate: String?, base: File?) {
            require(crate != null) {
                "vespera.crateName is not set: bundleNativeLib needs the Rust crate whose cdylib " +
                    "it bundles (or set vespera.bundleNative = false when the library comes from " +
                    "per-platform native jars)."
            }
            require(base != null) {
                "Neither vespera.cargoRoot nor vespera.targetDir is set: bundleNativeLib cannot " +
                    "locate the '$crate' cdylib. Set one of them, or vespera.bundleNative = false " +
                    "when the library comes from per-platform native jars."
            }
        }

        /**
         * Prebuilt-only bundling (`extraNativeLibs` without `cargoRoot` /
         * `targetDir`): fail when the directory holds no library at all,
         * where the `Sync` would be NO-SOURCE and the jar ship none.
         */
        fun requirePrebuilt(dir: File) {
            require(File(dir, "native").walk().any { it.isFile }) {
                "vespera.extraNativeLibs ($dir) holds no native/<platform>/<lib> file and neither " +
                    "vespera.cargoRoot nor vespera.targetDir is set: the jar would ship no native " +
                    "library."
            }
        }

        /** The embedded default `bridgeVersion`, or `null` when the resource is missing. */
        fun embeddedBridgeVersion(): String? =
            VesperaBridgePlugin::class.java.getResourceAsStream(VERSION_RESOURCE)?.use { input ->
                Properties().apply { load(input) }.getProperty("bridgeVersion")
                    ?.trim()?.takeIf { it.isNotEmpty() }
            }
    }
}
