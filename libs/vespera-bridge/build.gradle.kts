plugins {
    `java-library`
    `maven-publish`
    signing
}

group = "io.github.dev-five-git"
version = "0.0.4"

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
    withSourcesJar()
    withJavadocJar()
}

tasks.withType<Javadoc> {
    options.encoding = "UTF-8"
    (options as StandardJavadocDocletOptions).addStringOption("Xdoclint:none", "-quiet")
}

tasks.withType<JavaCompile> {
    options.encoding = "UTF-8"
}

repositories {
    mavenCentral()
}

dependencies {
    api("org.springframework.boot:spring-boot-starter-web:3.2.5")
    api("com.fasterxml.jackson.core:jackson-databind:2.17.0")
}

publishing {
    publications {
        create<MavenPublication>("mavenJava") {
            from(components["java"])

            groupId = project.group.toString()
            artifactId = "vespera-bridge"
            version = project.version.toString()

            pom {
                name.set("vespera-bridge")
                description.set("JNI bridge for Rust vespera engine — drop-in Spring proxy with single-JAR deployment")
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
    }

    repositories {
        val ghUser = System.getenv("GITHUB_ACTOR")
        val ghToken = System.getenv("GITHUB_TOKEN")
        maven {
            name = "GitHubPackages"
            url = uri("https://maven.pkg.github.com/dev-five-git/vespera")
            credentials {
                username = ghUser
                password = ghToken
            }
        }
    }
}

signing {
    val signingKey = System.getenv("SIGNING_KEY")
    val signingPassword = System.getenv("SIGNING_PASSWORD")

    if (!signingKey.isNullOrBlank() && !signingPassword.isNullOrBlank()) {
        useInMemoryPgpKeys(signingKey, signingPassword)
        sign(publishing.publications)
    }
}