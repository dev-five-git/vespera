pluginManagement {
    repositories {
        mavenLocal()
        gradlePluginPortal()
        mavenCentral()
    }
    // demo-app applies `kr.devfive.vespera-bridge` WITHOUT a version so it
    // always resolves the plugin built from this repository — CI publishes it
    // to mavenLocal right before this build, and the plugin is not on the
    // Gradle Plugin Portal, so a hard-coded pin becomes unresolvable the moment
    // the in-repo version is bumped.  `plugins { id(..) version ".." }` cannot
    // take an expression, hence the resolution strategy.  Real consumers pin a
    // released version instead.
    resolutionStrategy {
        eachPlugin {
            if (requested.id.id == "kr.devfive.vespera-bridge") {
                val buildScript = java.io.File(
                    settingsDir,
                    "../../../libs/vespera-bridge-gradle-plugin/build.gradle.kts",
                )
                useVersion(
                    Regex("(?m)^version\\s*=\\s*\"([^\"]+)\"")
                        .find(buildScript.readText())
                        ?.groupValues
                        ?.get(1)
                        ?: error("No `version = \"...\"` found in $buildScript"),
                )
            }
        }
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
