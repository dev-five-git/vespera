plugins {
    `java-library`
    id("com.vanniktech.maven.publish") version "0.36.0"
}

group = "kr.devfive"
version = "0.0.15"

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

    testImplementation("org.junit.jupiter:junit-jupiter:5.10.2")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher:1.10.2")
}

tasks.named<Test>("test") {
    useJUnitPlatform()
}

// Gate Maven Central signing on the presence of in-memory signing
// credentials so `publishToMavenLocal` works for development /
// dogfooding without GPG keys, while production releases still sign.
val shouldSign = !providers.gradleProperty("signingInMemoryKeyId").orNull.isNullOrBlank()
        || !System.getenv("ORG_GRADLE_PROJECT_signingInMemoryKeyId").isNullOrBlank()

mavenPublishing {
    publishToMavenCentral(automaticRelease = true)
    if (shouldSign) signAllPublications()

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
