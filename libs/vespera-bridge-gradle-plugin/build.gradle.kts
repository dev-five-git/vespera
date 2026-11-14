import com.vanniktech.maven.publish.GradlePublishPlugin

plugins {
    `java-gradle-plugin`
    `kotlin-dsl`
    id("com.vanniktech.maven.publish") version "0.37.0"
    // Gradle Plugin Portal publishing (`publishPlugins` task). Credentials
    // come from GRADLE_PUBLISH_KEY / GRADLE_PUBLISH_SECRET env vars in CI.
    id("com.gradle.plugin-publish") version "2.2.1"
    // CycloneDX SBOM of the plugin JAR (see "SBOM" at the end of this file).
    id("org.cyclonedx.bom") version "3.5.1"
}

// Fail LOUDLY when the Plugin Portal release is actually requested without
// credentials. The previous `onlyIf` silently SKIPPED `publishPlugins`, so a
// release with missing secrets reported success while never reaching the
// Portal. The check runs only when `publishPlugins` executes, so local builds,
// tests and `publishToMavenLocal` (the demo-app dogfooding path) are unaffected.
// Credentials: GRADLE_PUBLISH_KEY / GRADLE_PUBLISH_SECRET env vars (CI) or the
// `gradle.publish.key` / `gradle.publish.secret` Gradle properties.
val pluginPortalKey = providers.environmentVariable("GRADLE_PUBLISH_KEY")
    .orElse(providers.gradleProperty("gradle.publish.key"))
val pluginPortalSecret = providers.environmentVariable("GRADLE_PUBLISH_SECRET")
    .orElse(providers.gradleProperty("gradle.publish.secret"))

tasks.named("publishPlugins") {
    doFirst("require Gradle Plugin Portal credentials") {
        if (pluginPortalKey.orNull.isNullOrBlank() || pluginPortalSecret.orNull.isNullOrBlank()) {
            throw GradleException(
                "publishPlugins requires Gradle Plugin Portal credentials: set " +
                    "GRADLE_PUBLISH_KEY and GRADLE_PUBLISH_SECRET (or the gradle.publish.key / " +
                    "gradle.publish.secret Gradle properties) to non-blank values.",
            )
        }
    }
}

group = "kr.devfive"
version = "0.8.1"

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
}

kotlin {
    jvmToolchain(17)
}

repositories {
    mavenCentral()
    gradlePluginPortal()
}

dependencies {
    testImplementation("org.junit.jupiter:junit-jupiter:6.1.3")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher:6.1.3")
}

tasks.test {
    useJUnitPlatform()
}

// Default `vespera.bridgeVersion`: the kr.devfive:vespera-bridge version of
// this same repository commit, read from the sibling bridge build script and
// embedded as a classpath resource next to VesperaBridgePlugin. A plugin
// release therefore defaults to the bridge release it was built (and tested)
// with; consumers can still pin another one explicitly.
val embeddedBridgeVersion: Provider<String> = providers
    .fileContents(layout.projectDirectory.file("../vespera-bridge/build.gradle.kts"))
    .asText
    .map { script ->
        Regex("(?m)^version\\s*=\\s*\"([^\"]+)\"").find(script)?.groupValues?.get(1)
            ?: throw GradleException("No `version = \"...\"` in ../vespera-bridge/build.gradle.kts")
    }

val generateBridgeVersionResource = tasks.register("generateBridgeVersionResource") {
    description = "Embeds the matching vespera-bridge version as the plugin's default bridgeVersion."
    val version = embeddedBridgeVersion
    val outputDir = layout.buildDirectory.dir("generated/bridgeVersionResource")
    inputs.property("bridgeVersion", version)
    outputs.dir(outputDir)
    doLast {
        val file = outputDir.get()
            .file("kr/devfive/vespera/vespera-bridge-plugin.properties").asFile
        file.parentFile.mkdirs()
        file.writeText("bridgeVersion=${version.get()}\n")
    }
}

sourceSets.main {
    resources.srcDir(generateBridgeVersionResource)
}

gradlePlugin {
    // Required by the Plugin Portal (`com.gradle.plugin-publish`).
    website.set("https://github.com/dev-five-git/vespera")
    vcsUrl.set("https://github.com/dev-five-git/vespera.git")

    plugins {
        create("vesperaBridge") {
            id = "kr.devfive.vespera-bridge"
            implementationClass = "kr.devfive.vespera.VesperaBridgePlugin"
            displayName = "Vespera Bridge"
            description = "Auto-wires Rust cdylib bundling + vespera-bridge dependency into a Spring Boot project."
            tags.set(listOf("rust", "jni", "vespera", "spring", "ffi"))
        }
    }
}

// Gate Maven Central signing on the presence of in-memory signing
// credentials so `publishToMavenLocal` works for development /
// dogfooding without GPG keys, while production releases still sign.
//
// We probe `signingInMemoryKey` (the actual PGP private key) rather
// than `signingInMemoryKeyId` because CI only sets the former — the
// vanniktech maven-publish plugin's `signAllPublications()` derives
// the key ID from the key bytes when no explicit ID is supplied.
val shouldSign = !providers.gradleProperty("signingInMemoryKey").orNull.isNullOrBlank()
        || !System.getenv("ORG_GRADLE_PROJECT_signingInMemoryKey").isNullOrBlank()

mavenPublishing {
    publishToMavenCentral(automaticRelease = true)
    if (shouldSign) signAllPublications()

    // `com.gradle.plugin-publish` owns the sources/javadoc jars in this
    // setup — vanniktech docs mandate GradlePublishPlugin (not GradlePlugin)
    // when both plugins are applied, to avoid duplicate jar registration.
    configure(GradlePublishPlugin())

    coordinates(
        groupId = "kr.devfive",
        artifactId = "vespera-bridge-gradle-plugin",
        version = project.version.toString(),
    )

    pom {
        name.set("vespera-bridge-gradle-plugin")
        description.set(
            "Gradle plugin that wires a Vespera Rust cdylib into a Java/Spring application — " +
                "auto-bundles the native library and adds the vespera-bridge dependency in one line."
        )
        url.set("https://github.com/dev-five-git/vespera")

        licenses {
            license {
                name.set("MIT License")
                url.set("https://opensource.org/licenses/MIT")
            }
        }

        developers {
            developer {
                id.set("owjs3901")
                name.set("devfive")
                email.set("contact@devfive.kr")
            }
        }

        scm {
            url.set("https://github.com/dev-five-git/vespera")
            connection.set("scm:git:git://github.com/dev-five-git/vespera.git")
            developerConnection.set("scm:git:ssh://git@github.com:dev-five-git/vespera.git")
        }
    }
}

// ── SBOM ──────────────────────────────────────────────────────────────────
// CycloneDX SBOM of what the plugin JAR pulls into a consumer's build (its
// runtimeClasspath; the Gradle API itself is provided by the consuming Gradle).
// Deliberately NOT attached to the `pluginMaven` publication: that publication
// is also what `publishPlugins` uploads, and the Gradle Plugin Portal rejects
// non-JAR (JSON) classified artifacts. The release job instead attests this
// file against the plugin JAR and uploads it as a GitHub release asset; CI
// (`sbom` job) builds it on every PR.
tasks.named<org.cyclonedx.gradle.CyclonedxDirectTask>("cyclonedxDirectBom") {
    includeConfigs = listOf("runtimeClasspath")
    xmlOutput.unsetConvention()
    jsonOutput.set(
        layout.buildDirectory.file("reports/cyclonedx-direct/${project.name}-${project.version}-cyclonedx.json"),
    )
}
