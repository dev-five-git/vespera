plugins {
    java
    id("org.springframework.boot") version "3.2.5"
    id("io.spring.dependency-management") version "1.1.4"
}

group = "kr.go.demo"
version = "0.1.0"

// ── Rust native library location ─────────────────────────────────────
// Point this to your cargo build output. Override with:
//   ./gradlew bootJar -PrustTarget=x86_64-unknown-linux-gnu
val repoRoot = rootProject.projectDir.resolve("../../..")
val rustRelease = repoRoot.resolve("target/release")

dependencies {
    implementation(files("${repoRoot}/libs/vespera-bridge/build/libs/vespera-bridge-0.1.0.jar"))
    implementation("org.springframework.boot:spring-boot-starter-web")
    testImplementation("org.springframework.boot:spring-boot-starter-test")
}

// ── Bundle native library into JAR ───────────────────────────────────
// Copies the Rust .dll/.so/.dylib into src/main/resources/native/{os}-{arch}/
// so VesperaBridge.loadBundled() can extract it at runtime.
tasks.register<Copy>("bundleNativeLib") {
    val os = detectOs()
    val arch = detectArch()
    val libName = mapLibraryName(os, "rust_jni_demo")

    from(rustRelease.resolve(libName))
    into(layout.buildDirectory.dir("resources/main/native/${os}-${arch}"))

    doFirst {
        val src = rustRelease.resolve(libName)
        require(src.exists()) {
            "Native library not found: $src\nRun: cargo build -p rust-jni-demo --release"
        }
    }
}

tasks.named("processResources") {
    dependsOn("bundleNativeLib")
}

tasks.test {
    useJUnitPlatform()
}

// ── Platform detection (mirrors VesperaBridge.java) ──────────────────
fun detectOs(): String {
    val os = System.getProperty("os.name", "").lowercase()
    return when {
        "win" in os -> "windows"
        "mac" in os || "darwin" in os -> "macos"
        else -> "linux"
    }
}

fun detectArch(): String {
    val arch = System.getProperty("os.arch", "").lowercase()
    return when {
        "amd64" in arch || "x86_64" in arch -> "x86_64"
        "aarch64" in arch || "arm64" in arch -> "aarch64"
        else -> arch
    }
}

fun mapLibraryName(os: String, name: String): String = when (os) {
    "windows" -> "$name.dll"
    "macos" -> "lib$name.dylib"
    else -> "lib$name.so"
}
