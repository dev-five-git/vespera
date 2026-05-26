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
    bridgeVersion.set("0.0.15")
}

dependencies {
    implementation("org.springframework.boot:spring-boot-starter-web")
    implementation("com.fasterxml.jackson.core:jackson-databind")
    testImplementation("org.springframework.boot:spring-boot-starter-test")
}

tasks.test {
    useJUnitPlatform()
}
