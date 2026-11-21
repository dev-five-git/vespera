plugins {
    java
    // Execution data for the bridge's reactive proxy, merged into the
    // libs/vespera-bridge JaCoCo report like demo-app's.
    jacoco
    id("org.springframework.boot") version "4.1.2"
    id("io.spring.dependency-management") version "1.1.7"
    // Same plugin as demo-app: bundles the rust_jni_demo cdylib and adds the
    // kr.devfive:vespera-bridge dependency (which brings NO transitive deps —
    // this app has WebFlux but no servlet API / Spring MVC on its classpath).
    id("kr.devfive.vespera-bridge")
}

group = "kr.go.demo"
version = "0.1.0"

// Dogfoods the bridge built from this repository (see demo-app).
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
    implementation("org.springframework.boot:spring-boot-starter-webflux")
    testImplementation("org.springframework.boot:spring-boot-starter-test")
}

// Same forwarding as demo-app: tuning knobs passed as `./gradlew -D...` reach
// the forked test JVM (the two builds are separate, so the list is repeated).
val forwardedSystemProperties = listOf(
    "vespera.bench",
    "vespera.streaming.chunkBytes",
    "vespera.streaming.channelCapacity",
    "vespera.runtime.workerThreads",
    "vespera.direct.maxRetainedBytes",
    "vespera.direct.maxBufferBytes",
    "vespera.native.dir",
)

tasks.test {
    useJUnitPlatform()
    forwardedSystemProperties.forEach { key ->
        System.getProperty(key)?.let { systemProperty(key, it) }
    }
    testLogging.showStandardStreams = true
}
