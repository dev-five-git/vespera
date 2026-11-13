plugins {
    java
    // Records execution data for the vespera-bridge classes this suite drives
    // through a REAL loaded cdylib — the JNI wrappers and the Spring proxy that
    // the bridge's own unit tests cannot reach. libs/vespera-bridge merges this
    // `build/jacoco/test.exec` into its report.
    jacoco
    id("org.springframework.boot") version "4.1.2"
    id("io.spring.dependency-management") version "1.1.7"
    // GraalVM native image, opt-in: `./gradlew -PnativeImage :demo-app:nativeCompile`
    // with GraalVM for JDK 25 as JAVA_HOME. Applied (below) only with -PnativeImage,
    // so the regular JVM build and the E2E suite never run Spring AOT. The version is
    // the one Spring Boot 4.1 manages. CI: .github/workflows/native-image.yml.
    id("org.graalvm.buildtools.native") version "1.1.14" apply false
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

if (providers.gradleProperty("nativeImage").isPresent) {
    apply(plugin = "org.graalvm.buildtools.native")
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
    // Swagger UI at /swagger-ui.html: the bridge's springdoc integration adds the
    // Rust apps' documents (vespera!(spec_url = "/openapi.json")) next to the
    // Java one. Optional for vespera-bridge users (springdoc 3.x for Boot 4).
    implementation("org.springdoc:springdoc-openapi-starter-webmvc-ui:3.2.0")
    testImplementation("org.springframework.boot:spring-boot-starter-test")
    // The `vespera` Actuator health indicator (BridgeHardeningHttpTest).
    testImplementation("org.springframework.boot:spring-boot-starter-actuator")
    testImplementation("org.springframework.boot:spring-boot-restclient")
    testImplementation("org.springframework.boot:spring-boot-resttestclient")
}

// Tests tagged `isolated-jvm` change process-wide native settings (e.g. the
// buffered-response cap, fixed on the first dispatch), so they run in their
// own JVM: excluded from `test`, run by `isolatedJvmTest`, which finalizes
// `test` so `:demo-app:test` (CI's jni-e2e job) always runs both.
val isolatedJvmTag = "isolated-jvm"
val isolatedJvmTest = tasks.register<Test>("isolatedJvmTest") {
    description = "Runs the E2E tests that need a JVM of their own."
    group = LifecycleBasePlugin.VERIFICATION_GROUP
    testClassesDirs = sourceSets.test.get().output.classesDirs
    classpath = sourceSets.test.get().runtimeClasspath
    useJUnitPlatform { includeTags(isolatedJvmTag) }
    testLogging.showStandardStreams = true
}
tasks.check { dependsOn(isolatedJvmTest) }

tasks.test {
    useJUnitPlatform { excludeTags(isolatedJvmTag) }
    finalizedBy(isolatedJvmTest)
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
