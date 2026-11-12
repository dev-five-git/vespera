package kr.devfive.vespera

import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.Optional
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations
import java.io.ByteArrayOutputStream
import javax.inject.Inject

/**
 * Runs one `cargo build` per planned native build (the host build plus each
 * `vespera.targets` cross-build). Never UP-TO-DATE: Cargo's own incremental
 * check is authoritative and cheap, and Gradle cannot see every input Cargo
 * reads (build scripts, path dependencies outside the root, env vars) — so
 * the task declares no file inputs either (fingerprinting the Rust sources
 * would be pure cost).
 */
abstract class CargoBuildTask : DefaultTask() {
    @get:Inject
    abstract val execOperations: ExecOperations

    /** Directory cargo runs in (the Cargo workspace root). */
    @get:Internal
    abstract val workingDirectory: DirectoryProperty

    /** `vespera.crateName` (package name, or its underscore library form). */
    @get:Input
    abstract val crateName: Property<String>

    /** `vespera.packageName`: explicit `cargo build -p` spec. */
    @get:Input
    @get:Optional
    abstract val packageName: Property<String>

    /** Cargo profile (`release`, `dev`, or a custom profile). */
    @get:Input
    abstract val profile: Property<String>

    /** Rust target triples to build; an empty string is the host build. */
    @get:Input
    abstract val triples: ListProperty<String>

    /** `CARGO_TARGET_DIR` override, when `vespera.targetDir` is set. */
    @get:Input
    @get:Optional
    abstract val cargoTargetDir: Property<String>

    init {
        outputs.upToDateWhen { false }
    }

    /**
     * The `cargo build -p` spec: `packageName`, a hyphenated `crateName`, or —
     * for an underscore `crateName` — the workspace package whose underscore
     * form it is (`cargo metadata`), else `crateName` unchanged.
     */
    fun resolvePackageSpec(): String {
        val crate = crateName.get()
        NativePlatform.knownPackageSpec(crate, packageName.orNull)?.let { return it }
        return NativePlatform.resolvePackageName(crate, cargoMetadata())
    }

    /** Full command lines, one per build (resolves the package spec). */
    fun plannedCommands(): List<List<String>> {
        val spec = resolvePackageSpec()
        return triples.get().map { triple ->
            NativePlatform.cargoCommand(spec, profile.get(), triple.ifEmpty { null })
        }
    }

    private fun cargoMetadata(): String? = try {
        val out = ByteArrayOutputStream()
        val err = ByteArrayOutputStream()
        // Resolved OUTSIDE the exec lambda: inside it, `workingDirectory` would
        // bind to the ExecSpec's own property (Gradle 9), not this task's.
        val root = workingDirectory.get().asFile
        val result = execOperations.exec {
            workingDir = root
            commandLine("cargo", "metadata", "--no-deps", "--format-version", "1", "--offline")
            standardOutput = out
            errorOutput = err
            isIgnoreExitValue = true
        }
        if (result.exitValue == 0) {
            out.toString(Charsets.UTF_8)
        } else {
            logger.warn("vespera: `cargo metadata` exited with {}; using crateName as the `cargo build -p` spec " +
                "(set vespera.packageName to skip the lookup): {}", result.exitValue, err.toString(Charsets.UTF_8).trim())
            null
        }
    } catch (e: Exception) {
        logger.warn("vespera: `cargo metadata` failed ({}); using crateName as the `cargo build -p` spec " +
            "(set vespera.packageName to skip the lookup)", e.toString())
        null
    }

    @TaskAction
    fun build() {
        val dir = workingDirectory.get().asFile
        val targetDir = cargoTargetDir.orNull
        plannedCommands().forEach { cmd ->
            execOperations.exec {
                workingDir = dir
                commandLine(cmd)
                if (targetDir != null) environment("CARGO_TARGET_DIR", targetDir)
            }
        }
    }
}
