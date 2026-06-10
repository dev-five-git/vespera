plugins {
    java
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
    id("kr.devfive.vespera-bridge") version "0.0.15"
}

group = "kr.go.demo"
version = "0.1.0"

vespera {
    crateName.set("rust_jni_demo")
    cargoRoot.set(rootProject.layout.projectDirectory.dir("../../.."))
    // Dogfoods the locally published bridge (./gradlew publishToMavenLocal
    // in libs/vespera-bridge) — required for the dispatchDirect E2E tests.
    bridgeVersion.set("0.1.1")
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
    ).forEach { key ->
        System.getProperty(key)?.let { systemProperty(key, it) }
    }
    // Bench output is read from stdout.
    testLogging.showStandardStreams = true
}
