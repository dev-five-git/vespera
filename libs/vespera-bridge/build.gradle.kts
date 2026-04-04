plugins {
    `java-library`
    id("com.vanniktech.maven.publish") version "0.36.0"
}

group = "kr.devfive"
version = "0.0.12"

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
    withSourcesJar()
}

tasks.withType<Javadoc>().configureEach {
    options.encoding = "UTF-8"
    (options as StandardJavadocDocletOptions).addStringOption("Xdoclint:none", "-quiet")
}

tasks.withType<JavaCompile>().configureEach {
    options.encoding = "UTF-8"
}

repositories {
    mavenCentral()
}

dependencies {
    api("org.springframework.boot:spring-boot-starter-web:3.2.5")
    api("com.fasterxml.jackson.core:jackson-databind:2.17.0")
}

// TODO: Remove after confirming CI secrets work
logger.lifecycle("=== Maven Central Publish Debug ===")
listOf(
    "mavenCentralUsername",
    "mavenCentralPassword",
    "signingInMemoryKeyId",
    "signingInMemoryKey",
    "signingInMemoryKeyPassword",
).forEach { key ->
    val value = providers.gradleProperty(key).orNull
    val status = when {
        value == null -> "MISSING"
        value.isBlank() -> "EMPTY"
        else -> "OK (${value.length} chars)"
    }
    logger.lifecycle("  [ENV CHECK] $key = $status")
}
logger.lifecycle("==================================")

mavenPublishing {
    publishToMavenCentral(automaticRelease = true)
    signAllPublications()

    coordinates(
        groupId = "kr.devfive",
        artifactId = "vespera-bridge",
        version = project.version.toString(),
    )

    pom {
        name.set("vespera-bridge")
        description.set("JNI bridge for Rust vespera engine - drop-in Spring proxy with single-JAR deployment")
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
