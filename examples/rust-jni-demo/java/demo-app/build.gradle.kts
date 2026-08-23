plugins {
    java
    // Records execution data for the vespera-bridge classes this suite drives
    // through a REAL loaded cdylib — the JNI wrappers and the Spring proxy that
    // the bridge's own unit tests cannot reach. libs/vespera-bridge merges this
    // `build/jacoco/test.exec` into its report.
    jacoco
    id("org.springframework.boot") version "3.2.5"
    id("io.spring.dependency-management") version "1.1.4"
    // ───────────────────────────────────────────────────────────────────
    // Vespera bridge plugin — auto-wires:
    //   - bundleNativeLib task (cdylib → resources/native/<os>-<arch>/)
    //   - processResources dependency on bundleNativeLib
    //   - kr.devfive:vespera-bridge implementation dep
    //
    // Before this plugin: 22 lines of boilerplate (Copy task, OS/arch
    // detection helpers, library-name mapping, processResources wiring).
    // After: the 5-line `vespera { ... }` block below.
    // ───────────────────────────────────────────────────────────────────
    // Version resolved in settings.gradle.kts from the in-repo plugin build.
    id("kr.devfive.vespera-bridge")
}

group = "kr.go.demo"
version = "0.1.0"

// This example dogfoods the bridge built from *this* repository
// (`./gradlew publishToMavenLocal` in libs/vespera-bridge), so the version is
// read from that module's build script instead of being pinned here — a hard
// coded pin silently falls back to the last release on Maven Central whenever
// the in-repo bridge is bumped, and the E2E tests then compile against a stale
// API. Real consumers pin a released version (see libs/vespera-bridge/README).
val bridgeBuildScript = rootProject.layout.projectDirectory
    .file("../../../libs/vespera-bridge/build.gradle.kts")
val localBridgeVersion: String =
    Regex("(?m)^version\\s*=\\s*\"([^\"]+)\"")
        .find(providers.fileContents(bridgeBuildScript).asText.get())
        ?.groupValues?.get(1)
        ?: error("No `version = \"...\"` found in ${bridgeBuildScript.asFile}")

vespera {
    crateName.set("rust_jni_demo")
    cargoRoot.set(rootProject.layout.projectDirectory.dir("../../.."))
    bridgeVersion.set(localBridgeVersion)
}

dependencies {
    implementation("org.springframework.boot:spring-boot-starter-web")
    implementation("com.fasterxml.jackson.core:jackson-databind")
    testImplementation("org.springframework.boot:spring-boot-starter-test")
}

tasks.test {
    useJUnitPlatform()
    // Propagate streaming bench knobs from the Gradle CLI into the
    // forked test JVM (chunk size is process-fixed, so each value
    // needs its own `gradlew test -D...` run).
    listOf(
        "vespera.bench",
        "vespera.streaming.chunkBytes",
        "vespera.streaming.channelCapacity",
        "vespera.runtime.workerThreads",
        "vespera.direct.maxRetainedBytes",
        "vespera.direct.maxBufferBytes",
    ).forEach { key ->
        System.getProperty(key)?.let { systemProperty(key, it) }
    }
    // Bench output is read from stdout.
    testLogging.showStandardStreams = true
}
