import com.vanniktech.maven.publish.GradlePublishPlugin

plugins {
    `java-gradle-plugin`
    `kotlin-dsl`
    id("com.vanniktech.maven.publish") version "0.37.0"
    // Gradle Plugin Portal publishing (`publishPlugins` task). Credentials
    // come from GRADLE_PUBLISH_KEY / GRADLE_PUBLISH_SECRET env vars in CI.
    id("com.gradle.plugin-publish") version "2.2.1"
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
version = "0.5.0"

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
