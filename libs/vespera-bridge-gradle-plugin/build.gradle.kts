plugins {
    `java-gradle-plugin`
    `kotlin-dsl`
    id("com.vanniktech.maven.publish") version "0.36.0"
}

group = "kr.devfive"
version = "0.1.1"

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

gradlePlugin {
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
