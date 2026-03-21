plugins {
    `java-library`
    `maven-publish`
}

group = "com.devfive.vespera"
version = "0.1.0"

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
    withSourcesJar()
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
        create<MavenPublication>("maven") {
            from(components["java"])

            pom {
                name.set("vespera-bridge")
                description.set("JNI bridge for Rust vespera engine — drop-in Spring proxy with single-JAR deployment")
                url.set("https://github.com/dev-five-git/vespera")
                licenses {
                    license {
                        name.set("MIT")
                        url.set("https://github.com/dev-five-git/vespera/blob/main/LICENSE")
                    }
                }
            }
        }
    }
    repositories {
        maven {
            name = "GitHubPackages"
            url = uri("https://maven.pkg.github.com/dev-five-git/vespera")
            credentials {
                username = System.getenv("GITHUB_ACTOR") ?: ""
                password = System.getenv("GITHUB_TOKEN") ?: ""
            }
        }
    }
}
