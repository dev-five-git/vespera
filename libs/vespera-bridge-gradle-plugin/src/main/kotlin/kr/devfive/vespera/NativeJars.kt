package kr.devfive.vespera

import org.gradle.api.Action
import org.gradle.api.Project
import org.gradle.api.Task
import org.gradle.api.publish.PublishingExtension
import org.gradle.api.publish.maven.MavenPublication
import org.gradle.api.tasks.TaskProvider
import org.gradle.jvm.tasks.Jar
import java.io.File

/**
 * Per-platform native jars (`vespera.nativeJarPlatforms`): one classifier jar
 * per platform holding `native/<platform>/` of `vespera.nativeJarsDir`, plus
 * the `vesperaNative` Maven publication when `maven-publish` is applied.
 */
internal object NativeJars {
    /** Lifecycle task building every native jar. */
    const val AGGREGATE_TASK = "nativeJars"

    /** Maven publication carrying the native jars as classifier artifacts. */
    const val PUBLICATION = "vesperaNative"

    /** `linux-x86_64-musl` → `nativeJarLinuxX86_64Musl`. */
    fun taskName(platform: String): String =
        "nativeJar" + platform.split('-').joinToString("") { part ->
            part.replaceFirstChar { it.uppercaseChar() }
        }

    /**
     * Fail fast (with the plain-value [dir] / [libFile] captured at
     * configuration) when the platform folder of a native jar is missing or
     * lacks the expected library.
     */
    fun requireLibrary(platform: String, dir: File?, libFile: String?) {
        requireNotNull(dir) {
            "vespera.nativeJarPlatforms needs vespera.nativeJarsDir (or extraNativeLibs): the " +
                "directory holding native/<platform>/<lib> (for example downloaded CI artifacts)."
        }
        val folder = File(dir, "native/$platform")
        val files = folder.listFiles()?.filter { it.isFile }.orEmpty()
        val present = files.map { it.name }.sorted()
        if (libFile != null) {
            require(libFile in present) {
                "Native jar '$platform': ${File(folder, libFile)} not found" +
                    (if (present.isEmpty()) "" else " (the folder holds $present)") +
                    ". Put the $platform build of the cdylib there before packaging."
            }
        } else {
            require(present.isNotEmpty()) {
                "Native jar '$platform': no library under $folder. Put the $platform build of " +
                    "the cdylib there before packaging."
            }
        }
    }

    fun register(project: Project, ext: VesperaBridgeExtension) {
        val aggregate = project.tasks.register(AGGREGATE_TASK, Action<Task> {
            group = "vespera"
            description = "Build one native jar per vespera.nativeJarPlatforms entry."
        })
        val sourceDir = ext.nativeJarsDir.orElse(ext.extraNativeLibs)
        val version = project.provider { project.version.toString() }
        val libsDir = project.layout.buildDirectory.dir("libs")

        ext.nativeJarPlatforms.all(Action<String> {
            val platform = this
            val os = NativePlatform.osOfPlatformKey(platform)
            val jar: TaskProvider<Jar> = project.tasks.register(taskName(platform), Jar::class.java, Action<Jar> {
                group = "vespera"
                description = "Package native/$platform/ as the '$platform' native jar."
                archiveBaseName.convention(project.name)
                archiveVersion.convention(version)
                archiveClassifier.set(platform)
                destinationDirectory.convention(libsDir)
                from(sourceDir) { include("native/$platform/**") }
                // Plain values only: configuration-cache safe.
                val dir = sourceDir.orNull?.asFile
                val libFile = ext.crateName.orNull?.let {
                    NativePlatform.mapLibraryName(os, NativePlatform.libraryStem(it, ext.libName.orNull))
                }
                doFirst(Action<Task> { requireLibrary(platform, dir, libFile) })
            })
            aggregate.configure(Action<Task> { dependsOn(jar) })
        })

        project.pluginManager.withPlugin("maven-publish") {
            val publishing = project.extensions.getByType(PublishingExtension::class.java)
            ext.nativeJarPlatforms.all(Action<String> {
                val platform = this
                val publication = publishing.publications.findByName(PUBLICATION) as MavenPublication?
                    ?: publishing.publications.create(PUBLICATION, MavenPublication::class.java)
                publication.artifact(project.tasks.named(taskName(platform)))
            })
        }
    }
}
