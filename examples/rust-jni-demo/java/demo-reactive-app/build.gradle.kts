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

tasks.test {
    useJUnitPlatform()
    testLogging.showStandardStreams = true
}
