plugins {
    `java-library`
    jacoco
    id("com.vanniktech.maven.publish") version "0.37.0"
}

group = "kr.devfive"
version = "0.5.0"

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

// The configuration processor merges additional-spring-configuration-metadata.json
// from the processed resources, so they must exist before compileJava runs.
tasks.named<JavaCompile>("compileJava") {
    inputs.files(tasks.named("processResources"))
}

// Embed the bridge version for VesperaVersions (bridge <-> native release check).
val bridgeVersion = project.version.toString()
tasks.named<ProcessResources>("processResources") {
    inputs.property("bridgeVersion", bridgeVersion)
    filesMatching("**/vespera-bridge.properties") {
        expand("version" to bridgeVersion)
    }
}

repositories {
    mavenCentral()
}

// ── Dependency model (BREAKING after 0.5.0 — see README "Dependencies") ──
//
// Spring is PROVIDED by the consuming application, not dragged in by the
// bridge: everything Spring/servlet/slf4j is `compileOnly`, so the published
// POM lists NO Spring dependencies.  Every Spring Boot app already has
// spring-boot-autoconfigure + spring-webmvc + a servlet API + slf4j on its
// classpath via spring-boot-starter-web(mvc); a non-Spring consumer (plain
// VesperaBridge / codec usage) gets a JAR with zero transitive dependencies.
// The Spring-typed classes (proxy controller, autoconfiguration, resolvers)
// are only loaded through the Spring Boot autoconfiguration, which is
// @ConditionalOnClass-guarded.
//
// Compile target: Spring Boot 4.1 / Spring Framework 7.0.  Runtime support
// for Spring Boot 3.5 / Framework 6.2 is verified by the `testBoot3` task.
val springBootVersion = "4.1.2"
val springFrameworkVersion = "7.0.10"
val springBoot3Version = "3.5.16"
val springFramework3Version = "7.0.10"
val micrometerVersion = "1.15.12"

dependencies {
    compileOnly("org.springframework.boot:spring-boot-autoconfigure:$springBootVersion")
    compileOnly("org.springframework:spring-webmvc:$springFrameworkVersion")
    compileOnly("jakarta.servlet:jakarta.servlet-api:6.1.0")
    compileOnly("org.slf4j:slf4j-api:2.0.17")
    // Optional: MicrometerDispatchObserver / VesperaBridgeMetricsAutoConfiguration
    // only load when the application ships Micrometer (e.g. via Actuator).
    compileOnly("io.micrometer:micrometer-core:$micrometerVersion")
    // Optional: the `vespera` Actuator health indicator. Boot 4 moved the health
    // API to spring-boot-health (org.springframework.boot.health.contributor);
    // Boot 3.5 keeps it in spring-boot-actuator (org.springframework.boot.actuate
    // .health). Both are compiled against (non-transitively, so the Boot 3 jar
    // does not drag Boot 3 core onto the compile classpath) and each indicator
    // is only loaded behind a class-name guard.
    compileOnly("org.springframework.boot:spring-boot-health:$springBootVersion")
    compileOnly("org.springframework.boot:spring-boot-actuator:$springBoot3Version") {
        isTransitive = false
    }

    // IDE metadata for `vespera.bridge.*`: generates
    // META-INF/spring-configuration-metadata.json (field Javadoc becomes the
    // description) and merges src/main/resources/META-INF/
    // additional-spring-configuration-metadata.json (value hints, properties
    // read only through @ConditionalOnProperty). Compile-time only — the
    // generated JSON is a plain resource, valid for Boot 3.5 and 4 alike.
    annotationProcessor("org.springframework.boot:spring-boot-configuration-processor:$springBootVersion")

    testImplementation("org.springframework.boot:spring-boot-starter-web:$springBootVersion")
    testImplementation("io.micrometer:micrometer-core:$micrometerVersion")
    testImplementation("org.springframework.boot:spring-boot-health:$springBootVersion")
    // VesperaWireTest cross-checks the hand-rolled JSON codec against Jackson.
    testImplementation("com.fasterxml.jackson.core:jackson-databind:2.23.0")
    testImplementation("org.junit.jupiter:junit-jupiter:6.1.3")
    // MockHttpServletRequest for resolver unit tests (no servlet container).
    testImplementation("org.springframework:spring-test:$springFrameworkVersion")
    // WebApplicationContextRunner for autoconfigure branch tests
    // (its AssertableWebApplicationContext implements AssertJ's
    // AssertProvider, so assertj-core must be on the test classpath).
    testImplementation("org.springframework.boot:spring-boot-test:$springBootVersion")
    testImplementation("org.assertj:assertj-core:3.27.7")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher:6.1.3")
}

// ── Spring Boot 3.5 compatibility run ──────────────────────────────────
// Runs the SAME compiled main + test classes (built against Boot 4 /
// Framework 7) on a Spring Boot 3.5 / Framework 6.2 runtime classpath, so any
// Boot-4-only API the bridge links against fails here as NoSuchMethodError /
// NoClassDefFoundError instead of in a user's Boot 3 app.  The test sources
// must therefore stick to Spring TEST APIs present in both lines (e.g.
// `HttpHeaders.getFirst`, not the Framework-7-only `containsHeader`).
val testBoot3Runtime: Configuration = configurations.create("testBoot3Runtime") {
    isCanBeConsumed = false
    isCanBeResolved = true
}

dependencies {
    testBoot3Runtime("org.springframework.boot:spring-boot-starter-web:$springBoot3Version")
    testBoot3Runtime("io.micrometer:micrometer-core:$micrometerVersion")
    testBoot3Runtime("org.springframework.boot:spring-boot-actuator:$springBoot3Version")
    testBoot3Runtime("org.springframework.boot:spring-boot-test:$springBoot3Version")
    testBoot3Runtime("org.springframework:spring-test:$springFramework3Version")
    testBoot3Runtime("com.fasterxml.jackson.core:jackson-databind:2.23.0")
    testBoot3Runtime("org.junit.jupiter:junit-jupiter:6.1.3")
    testBoot3Runtime("org.assertj:assertj-core:3.27.7")
    testBoot3Runtime("org.junit.platform:junit-platform-launcher:6.1.3")
}

val testBoot3 = tasks.register<Test>("testBoot3") {
    description = "Runs the unit suite against a Spring Boot $springBoot3Version runtime."
    group = LifecycleBasePlugin.VERIFICATION_GROUP
    useJUnitPlatform()
    testClassesDirs = sourceSets["test"].output.classesDirs
    classpath = sourceSets["main"].output + sourceSets["test"].output + testBoot3Runtime
    shouldRunAfter(tasks.named("test"))
}

// Run it whenever the unit suite runs (CI invokes `./gradlew test`), so a
// Boot-4-only API reference fails the build rather than a Boot 3 user.
tasks.named("test") {
    finalizedBy(testBoot3)
}

tasks.named("check") {
    dependsOn(testBoot3)
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
