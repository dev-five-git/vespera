pluginManagement {
    repositories {
        mavenLocal()
        gradlePluginPortal()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    // PREFER_SETTINGS — ignore any repositories Spring Boot or other
    // plugins may declare at the project level.  Dogfooding the local
    // build of vespera-bridge requires mavenLocal() to be visible
    // even when Spring Boot would otherwise add only mavenCentral.
    @Suppress("UnstableApiUsage")
    repositoriesMode.set(RepositoriesMode.PREFER_SETTINGS)
    repositories {
        mavenLocal()
        mavenCentral()
    }
}

rootProject.name = "vespera-jni-demo"

include("demo-app")
