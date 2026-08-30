plugins {
    `java-library`
    jacoco
    id("com.vanniktech.maven.publish") version "0.36.0"
}

group = "kr.devfive"
version = "0.4.0"

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
    // MockHttpServletRequest for resolver unit tests (no servlet container).
    testImplementation("org.springframework:spring-test:6.1.6")
    // WebApplicationContextRunner for autoconfigure branch tests
    // (its AssertableWebApplicationContext implements AssertJ's
    // AssertProvider, so assertj-core must be on the test classpath).
    testImplementation("org.springframework.boot:spring-boot-test:3.2.5")
    testImplementation("org.assertj:assertj-core:3.25.3")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher:1.10.2")
}

tasks.named<Test>("test") {
    useJUnitPlatform()
    // Opt-in micro-benchmarks (PerfAllocBench, gated by @EnabledIfSystemProperty)
    // read this property; propagate it from the Gradle CLI into the forked test
    // JVM — same pattern as the rust-jni-demo demo-app.
    System.getProperty("vespera.bench")?.let { systemProperty("vespera.bench", it) }
    finalizedBy(tasks.named("jacocoTestReport"))
}

// The JNI wrappers and the Spring proxy only execute with a real cdylib loaded,
// which this module's unit tests cannot do. That code IS exercised — by the
// demo-app E2E suite in examples/rust-jni-demo/java, a SEPARATE Gradle build.
// Merging its execution data (when present) makes the report describe what
// actually ran rather than only what the unit suite can reach; the class IDs
// match because demo-app resolves this exact build's JAR from mavenLocal.
val demoAppExecutionData = rootProject.layout.projectDirectory
    .file("../../examples/rust-jni-demo/java/demo-app/build/jacoco/test.exec")

tasks.named<JacocoReport>("jacocoTestReport") {
    dependsOn(tasks.named("test"))
    executionData.setFrom(
        files(
            layout.buildDirectory.file("jacoco/test.exec"),
            demoAppExecutionData,
        ).filter { it.exists() },
    )
    reports {
        xml.required.set(true)
        html.required.set(true)
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
