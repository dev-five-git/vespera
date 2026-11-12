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
import javax.inject.Inject

/**
 * Runs one `cargo build` per planned native build (the host build plus each
 * `vespera.targets` cross-build). Never UP-TO-DATE: Cargo's own incremental
 * check is authoritative and cheap, and Gradle cannot see every input Cargo
 * reads (build scripts, path dependencies outside the root, env vars).
 */
abstract class CargoBuildTask : DefaultTask() {
    @get:Inject
    abstract val execOperations: ExecOperations

    /** Directory cargo runs in (the Cargo workspace root). */
    @get:Internal
    abstract val workingDirectory: DirectoryProperty

    /** Full command lines, one per build. */
    @get:Input
    abstract val commands: ListProperty<List<String>>

    /** `CARGO_TARGET_DIR` override, when `vespera.targetDir` is set. */
    @get:Input
    @get:Optional
    abstract val cargoTargetDir: Property<String>

    init {
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun build() {
        val dir = workingDirectory.get().asFile
        val targetDir = cargoTargetDir.orNull
        commands.get().forEach { cmd ->
            execOperations.exec {
                workingDir = dir
                commandLine(cmd)
                if (targetDir != null) environment("CARGO_TARGET_DIR", targetDir)
            }
        }
    }
}
