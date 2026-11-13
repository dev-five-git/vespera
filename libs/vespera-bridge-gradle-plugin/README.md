# vespera-bridge-gradle-plugin

Gradle plugin `kr.devfive.vespera-bridge` (Maven Central / Plugin Portal: `kr.devfive:vespera-bridge-gradle-plugin`). It bundles your Rust cdylib into the jar under `native/<platform>/`, adds the matching `kr.devfive:vespera-bridge` dependency, and packages per-platform native jars. Full setup guide: [vespera-bridge README → One-line setup via the Gradle plugin](../vespera-bridge/README.md#one-line-setup-via-the-gradle-plugin-recommended).

## DSL

| Property | Default | Purpose |
|---|---|---|
| `crateName` | — (required) | Cargo package (`my-app`) or its underscore form; selects the library file name |
| `packageName` / `libName` | derived | `cargo build -p` spec / custom `[lib] name` |
| `cargoRoot`, `targetDir`, `cargoProfile` | `<cargoRoot>/target`, `release` | where the cdylib is read from |
| `autoBuildCargo` | `false` | run `cargoBuild` before `bundleNativeLib` |
| `targets` | empty | extra cross builds: platform → Rust triple |
| `extraNativeLibs` | unset | prebuilt `native/<platform>/<lib>` merged into the app jar |
| `bundleNative` | `true` | `false`: `processResources` bundles nothing (the app takes its library from native jars) |
| `nativeJarPlatforms` | empty | platforms to package as classifier jars (`nativeJar<Platform>`, `nativeJars`) |
| `nativeJarsDir` | `extraNativeLibs` | source of the native jars (`native/<platform>/<lib>`) |
| `bridgeVersion` | the bridge release this plugin was built with | `vespera-bridge` dependency version |

## Per-platform native jars

```kotlin
plugins {
    `maven-publish`
    id("kr.devfive.vespera-bridge") version "<version>"
}
group = "com.acme"
version = "0.1.0"

vespera {
    crateName.set("my-app")
    bundleNative.set(false)
    nativeJarsDir.set(layout.projectDirectory.dir("prebuilt")) // prebuilt/native/<platform>/<lib>
    nativeJarPlatforms.addAll(listOf("linux-x86_64", "linux-aarch64", "macos-aarch64", "windows-x86_64"))
}
```

`./gradlew nativeJars` writes `build/libs/<project>-<version>-<platform>.jar` per platform (only `native/<platform>/`); `maven-publish` gets a `vesperaNative` publication with those classifier artifacts. Consumers add `runtimeOnly("com.acme:<project>:0.1.0:<platform>")` per deployment platform. A CI matrix template that builds the libraries and publishes the jars: [`.github/workflow-templates/vespera-native-jars.yml`](../../.github/workflow-templates/vespera-native-jars.yml). Details: [vespera-bridge README → Per-platform native jars](../vespera-bridge/README.md#per-platform-native-jars-publish-once-per-platform-consume-one).

## Development

```bash
./gradlew clean test   # unit + Gradle TestKit functional tests
```
