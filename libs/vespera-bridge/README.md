# vespera-bridge

JNI bridge that lets a Java/Spring application embed a Rust [`vespera`](../../) axum router in-process — no TCP, no JSON envelope overhead, raw bytes from end to end.

```xml
<dependency>
  <groupId>kr.devfive</groupId>
  <artifactId>vespera-bridge</artifactId>
  <version>0.6.0</version>
</dependency>
```

```kotlin
dependencies {
    implementation("kr.devfive:vespera-bridge:0.6.0")
}
```

### Version compatibility

- **Bridge ↔ Rust crate:** the `vespera-bridge` version must match the **minor** version of the `vespera` crate your cdylib is built with (bridge `0.6.x` ↔ `vespera = "0.6"`). The JNI symbol set and the binary wire format are only guaranteed stable within a minor line. Since 0.6 this is **checked at `VesperaBridge.init`**: the native library reports its crate version (`nativeVersion0`) and a different `major.minor` fails the init with an `IllegalStateException` naming both versions (patch levels may differ). `-Dvespera.native.versionCheck=warn` downgrades the failure to a WARNING, `off` skips the check; a native library built before 0.6 (no version symbol) only logs a WARNING.
- **Spring Boot:** compiled against Spring Boot 4.1 / Spring Framework 7.0; supported on **Spring Boot 3.5.x and 4.x** (Spring Framework 6.2 / 7.0, Jakarta Servlet 6.0+). The Boot 3.5 runtime is verified by the `testBoot3` Gradle task (run automatically after `./gradlew test`), which runs the unit suite against a Spring Boot 3.5 classpath. Spring Boot ≤ 3.4 / `javax.servlet` is not supported.
- **Java:** 17+.

### Dependencies — Spring is provided by your app

`vespera-bridge` declares **no transitive dependencies** (its POM is empty): Spring Boot, Spring MVC, the servlet API, Spring WebFlux / Reactor and SLF4J are compile-only and must come from the application. Every Spring Boot web app already has them through `spring-boot-starter-web` (Boot 3) / `spring-boot-starter-webmvc` (Boot 4), so nothing changes there. The autoconfiguration only activates in a servlet web application with Spring MVC on the classpath (`@ConditionalOnClass` + `@ConditionalOnWebApplication(type = SERVLET)`); its WebFlux counterpart only in a reactive web application with WebFlux on the classpath (see [WebFlux](#webflux)), and the non-Spring API (`VesperaBridge` dispatch / `encodeRequest` / `decodeResponse`) runs with no Spring jars at all.

Kotlin is optional too: the [Kotlin extensions](#kotlin) ship inside the same JAR, but `kotlin-stdlib` and `kotlinx-coroutines-core` are compile-only — Java applications never load a Kotlin class.

> Up to and including 0.5.0 the bridge exported `spring-boot-starter-web` and `jackson-databind` as `api` dependencies. If your build relied on getting either of them transitively from the bridge, declare it yourself.

### One-line setup via the Gradle plugin (recommended)

For Spring Boot apps the [`kr.devfive.vespera-bridge`](../vespera-bridge-gradle-plugin/) Gradle plugin replaces the ~22 lines of native-library-bundling boilerplate with a 5-line `vespera { ... }` block:

```kotlin
plugins {
    id("kr.devfive.vespera-bridge") version "0.6.0"
}

vespera {
    crateName.set("my-rust-lib")   // Cargo package name, or its underscore form "my_rust_lib"
    cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
    // bridgeVersion defaults to the vespera-bridge release this plugin version was built with;
    // set it only to pin a different bridge:  bridgeVersion.set("0.6.0")
}
```

- **`crateName`** — the Cargo package (`my-rust-lib`) or its underscore library form (`my_rust_lib`). The bundled file follows Cargo's naming: `-` becomes `_` (`my_rust_lib.dll` / `libmy_rust_lib.so` / `libmy_rust_lib.dylib`).
- **`packageName`** (optional) — the package passed to `cargo build -p` by `cargoBuild`. When unset, a hyphenated `crateName` is used as is; an underscore `crateName` is matched against `cargo metadata --no-deps` (e.g. `rust_jni_demo` → package `rust-jni-demo`), falling back to `crateName`.
- **`libName`** (optional) — the library file stem when `Cargo.toml` sets a custom `[lib] name`.
- **`bridgeVersion`** (optional) — defaults to the matching `kr.devfive:vespera-bridge` release; an explicit value wins.
- **`cargoSourceRoots`** is deprecated and ignored: `cargoBuild` is never `UP-TO-DATE` (Cargo's incremental check decides), so it no longer fingerprints Rust sources as Gradle inputs.
- The plugin is configuration-cache compatible.

The plugin auto-wires `bundleNativeLib` (cdylib → `resources/native/<os>-<arch>/`), `processResources` dependency, and the `vespera-bridge` `implementation` dependency.  See [`examples/rust-jni-demo/java/demo-app/build.gradle.kts`](../../examples/rust-jni-demo/java/demo-app/build.gradle.kts) for a real working example (the demo dogfoods the plugin end-to-end with a 1 MiB SHA256-verified bidirectional streaming round-trip).

#### Multi-platform jars

The plugin bundles the host build by default. To ship one jar for several platforms, cross-build extra Rust targets and/or merge libraries built elsewhere (e.g. CI artifacts):

```kotlin
vespera {
    // ...
    autoBuildCargo.set(true)                 // cargoBuild runs one `cargo build --target <triple>` per entry
    targets.put("linux-aarch64", "aarch64-unknown-linux-gnu")
    targets.put("macos-aarch64", "aarch64-apple-darwin")
    extraNativeLibs.set(layout.projectDirectory.dir("prebuilt")) // prebuilt/native/<os>-<arch>/<lib>
}
```

Each `targets` key is a bridge platform (`<os>-<arch>`, os `windows` / `macos` / `linux`); its library is read from `<targetDir>/<triple>/<profile>/` and bundled under `native/<os>-<arch>/` with that platform's file name (`.dll` / `lib*.dylib` / `lib*.so`). A plain `linux-<arch>` key built for a `*-linux-musl*` triple is bundled under `native/linux-<arch>-musl/` (where the bridge looks first on a musl JVM); `linux-<arch>-musl` keys are accepted too, so one jar can carry both builds, e.g. `targets.put("linux-x86_64-musl", "x86_64-unknown-linux-musl")` next to the glibc host build. A key naming the host platform replaces the host build (on an Alpine build host the host platform is `linux-<arch>-musl`). The cross toolchain/linker must already be installed. Everything under `extraNativeLibs/native/` is merged in; a locally built library wins over a prebuilt one with the same path. `cargoBuild` is never `UP-TO-DATE` (Cargo's own incremental check decides) and declares no file inputs.

#### Per-platform native jars (publish once per platform, consume one)

Vespera has no prebuilt native library of its own — the cdylib is **your** app — so instead of one fat jar you can publish one small classifier jar per platform and let each deployment pull only its own. Build the cdylib on a CI matrix (one native runner per platform), merge the artifacts into `prebuilt/native/<platform>/<lib>`, and package them from a dedicated Gradle project:

```kotlin
// myapp-native/build.gradle.kts
plugins {
    `maven-publish`
    id("kr.devfive.vespera-bridge") version "0.6.0"
}
group = "com.acme"
version = "0.1.0"

vespera {
    crateName.set("my-app")
    bundleNative.set(false)    // nothing is built or bundled locally
    nativeJarsDir.set(rootProject.layout.projectDirectory.dir("prebuilt")) // default: extraNativeLibs
    nativeJarPlatforms.addAll(listOf(
        "linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-aarch64", "windows-x86_64",
    ))
}
```

- One `nativeJar<Platform>` task per entry (`nativeJarLinuxX86_64`, `nativeJarLinuxX86_64Musl`, …; lifecycle task `nativeJars`) writes `build/libs/myapp-native-0.1.0-<platform>.jar` containing exactly `native/<platform>/<lib>`; it fails with the missing path when that platform's library is absent. Platforms are bridge platform keys (`<os>-<arch>`, `linux-<arch>-musl`).
- With `maven-publish` applied, the jars are the classifier artifacts of a `vesperaNative` publication (artifactId = project name, `pom` packaging, no main jar): `./gradlew publishVesperaNativePublicationTo<Repo>Repository`. Rename it with `publishing.publications.named<MavenPublication>("vesperaNative") { artifactId = "..." }`. With the vanniktech Maven Central plugin the publication is signed and uploaded with the others.
- **Consumers** keep the plugin (for the `vespera-bridge` dependency) with `bundleNative.set(false)` and add the platforms they deploy to — the bridge loads the host's from whichever jar carries it:

  ```kotlin
  vespera { crateName.set("my-app"); bundleNative.set(false) }
  dependencies {
      runtimeOnly("com.acme:myapp-native:0.1.0:linux-x86_64")   // production image
      runtimeOnly("com.acme:myapp-native:0.1.0:macos-aarch64")  // developer laptops
  }
  ```

A ready-to-copy GitHub Actions workflow (matrix: Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64 → merge → `nativeJars` → publish on tags) lives at [`.github/workflow-templates/vespera-native-jars.yml`](../../.github/workflow-templates/vespera-native-jars.yml).

## Two-line integration

```java
@SpringBootApplication
public class MyApp {
    public static void main(String[] args) {
        VesperaBridge.init("my_rust_lib");      // ← loads cdylib (bundled or system)
        SpringApplication.run(MyApp.class, args);
    }
}
```

> Do **not** add `com.devfive.vespera.bridge` to `@ComponentScan`. Everything
> here is registered by autoconfiguration (see below). Component-scanning the
> package instead picks `VesperaProxyController` up as a plain
> `@RestController` — it declares several constructors and no default one, so
> the context fails to start, and the scanned bean would also ignore
> `vespera.bridge.controller-enabled` and every `@ConditionalOnMissingBean`
> override documented under [Customization](#customization).

`VesperaProxyController` is **autoconfigured** (via Spring Boot `AutoConfiguration.imports`) and forwards every HTTP request to Rust.  You write zero controller code on the Java side, **zero `application.yml` config**, and **zero `import` lines** beyond the Spring Boot starter — the routes published in vespera's generated `openapi.json` are reachable at the same URLs through Spring.

## Zero-config defaults

Out of the box the autoconfigure module wires up:

| Concern | Default | Override |
|---|---|---|
| **App selection** | Read `X-Vespera-App` request header; absent → default app. **Client-controlled — restrict it with `vespera.bridge.exposed-apps`** (see [Multi-app routing → Security](#security-the-app-header-is-client-controlled)) | Properties `vespera.bridge.app-header` / `vespera.bridge.exposed-apps`, or custom [`AppNameResolver`](src/main/java/com/devfive/vespera/bridge/AppNameResolver.java) bean |
| **Dispatch mode** | [`SmartDispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/SmartDispatchModeResolver.java) since 0.2.0 — picks per request: [`DIRECT`](src/main/java/com/devfive/vespera/bridge/DispatchMode.java) (pooled direct buffers, no JNI array copies) for small/bodyless safe requests (GET/HEAD/OPTIONS, Content-Length absent or ≤ 1 MiB) ~2.2 µs; `SYNC` (heap-buffered) for small unsafe requests (POST/PUT/PATCH/DELETE ≤ 256 KiB) ~3.2 µs; `BIDIRECTIONAL_STREAMING` for the rest ~24.1 µs | Property `vespera.bridge.dispatch-mode: bidirectional-streaming` (opt out, restore pre-0.2.0 default), or custom [`DispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/DispatchModeResolver.java) bean |
| **URL pattern** | Single `@RequestMapping("/**")` catch-all — every vespera router URL exactly mirrors the published OpenAPI path | Properties `vespera.bridge.include-patterns` / `vespera.bridge.exclude-patterns` (see [Proxied paths](#proxied-paths-include--exclude-patterns)), or `vespera.bridge.controller-enabled: false` and your own controller |
| **Body handling** | Servlet `InputStream` straight through to Rust (no buffering) for streaming modes; full read for sync/async | (encoded by the chosen `DispatchMode`) |
| **Programmatic client** | A [`VesperaClient`](src/main/java/com/devfive/vespera/bridge/VesperaClient.java) bean for the default app (see [Programmatic client](#programmatic-client-vesperaclient)), also with `controller-enabled: false` | Your own `VesperaClient` bean (e.g. `VesperaClient.create().forApp("admin")`) |

Why `smart` as the default mode (since 0.2.0)? Measured on a small `GET /health` round-trip through the real JNI boundary the cheapest safe path per request is 7–11× cheaper than unconditional streaming:

| Request shape | Mode | ns/round-trip |
|---|---|---|
| Small/bodyless + safe (GET/HEAD/OPTIONS, Content-Length absent or ≤ 1 MiB) | `DIRECT` | ~2,200 |
| Small (≤ 256 KiB Content-Length) + unsafe (POST/PUT/PATCH/DELETE) | `SYNC` | ~3,200 |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | ~24,100 |

Trade-offs the new default makes on your behalf:

- **DIRECT** writes the wire response straight into a pooled direct `ByteBuffer` (per-thread, 64 KiB → `vespera.direct.maxBufferBytes` default 4 MiB). A response larger than the pooled buffer is kept natively and copied into a bigger buffer (or, beyond the cap and for `text/event-stream`, streamed) — the Rust handler runs once. Requests that `Accept` an event stream / NDJSON are streamed from the start.
- **SYNC** fully buffers the response on the JVM heap. The 256 KiB request-size gate keeps the response size reasonable for JSON-RPC-shaped traffic; large or unknown-length bodies still stream.
- **`BIDIRECTIONAL_STREAMING`** is unchanged for large/unknown-length bodies — multi-GB upload + multi-GB download still runs chunk-bounded, ~32 KiB resident each side.

The Spring endpoints **always** mirror vespera's `openapi.json` — `smart` picks the JNI path per request without any URL prefix or path-based heuristic that could diverge from the Rust router's view of the world.

Restore the pre-0.2.0 default (every request that may carry a body streams both ways, ~24 µs per round-trip uniform) with:

```yaml
vespera:
  bridge:
    dispatch-mode: bidirectional-streaming
```

## Customization

All defaults are individually replaceable.  Start with properties for minor tweaks, drop in a `@Bean` for serious behavior changes, or disable the autoconfigured controller entirely.

### 1. Tweak via `application.yml`

```yaml
vespera:
  bridge:
    app-header: X-My-App        # change the header that selects the app
    exposed-apps: [admin]       # apps the (client-controlled) app header may select
    controller-enabled: true     # set false to disable our controller
    include-patterns: ["/**"]    # paths the proxy serves (default /**), see "Proxied paths"
    exclude-patterns: []         # paths left to Spring MVC, e.g. ["/assets/**", "/*.html"]
    cors: rust                   # rust (default) | spring — who answers CORS, see "CORS"
    direct-retry-on-overflow: true # set false to avoid DIRECT retry double-execution
    forward-principal: true      # forward the authenticated principal to Rust, see "Forwarded principal"
    max-buffered-request-bytes: 67108864 # default 64 MiB; 0 = unlimited
    max-buffered-response-bytes: 67108864 # SYNC/ASYNC response cap → 500 (default 64 MiB; 0 = unlimited)
    native:                      # Rust-side knobs, applied before the first dispatch
      max-request-bytes: 67108864           # buffered request cap → 413 (0 = unlimited)
      max-streaming-request-bytes: 0        # streamed upload cap → 413 (0 = unlimited, default)
      max-buffered-response-bytes: 268435456 # overrides the forwarded max-buffered-response-bytes
      dispatch-timeout: 30s                 # deadline → 504 (default: disabled); see below
      async-max-in-flight: 4096             # dispatchAsync concurrency cap → 503
      worker-threads: 8                     # shared Tokio runtime workers (default: CPUs)
      max-blocking-threads: 512             # shared runtime blocking pool
```

`vespera.bridge.native.*` is forwarded to the `VesperaBridge.configure*`
methods during context startup, before the proxy serves a request. Unset keys
keep the native default (or the `vespera.limits.*` / `vespera.dispatch.*` /
`vespera.async.*` / `vespera.runtime.*` system properties and their `VESPERA_*`
environment variables). A value that arrives after the first native dispatch is
logged and ignored.

Precedence for every native knob (first hit wins, then process-fixed):
**programmatic `VesperaBridge.configure*` / `vespera.bridge.native.*`** &gt;
**system property** &gt; **`VESPERA_*` environment variable** &gt; built-in
default. Spring values only take effect when they are applied before the first
dispatch (the autoconfiguration does this during context startup). A
programmatic value equal to a knob's "unset" sentinel — `<= 0` for
`worker-threads` / `max-blocking-threads`, `< 0` for the limits, timeout and
async cap — is ignored and never shadows the system property or environment
variable (e.g. `configureRuntime(8, 0)` keeps
`-Dvespera.runtime.maxBlockingThreads`).

`dispatch-timeout` bounds the **whole dispatch** (handler + body) for the
buffered modes (`SYNC`, `ASYNC`, `DIRECT`) — expiry answers `504`. For the
streaming modes (`STREAMING`, `BIDIRECTIONAL_STREAMING`) it bounds only the
**time to the response header**: a handler that has not produced its header in
time answers `504`; once the header is committed the body streams without a
total deadline, so a long download that keeps making progress is never cut off.

A DIRECT response that overflows the pooled buffer is **kept on the Rust side**
(the overflow stash) and fetched into a grown buffer — the handler runs once.
A response beyond the pool cap (`vespera.direct.maxBufferBytes`) or a
`text/event-stream` response is streamed to the client from the stash instead.
`vespera.bridge.direct-retry-on-overflow` (default `true`) now only matters when
the stash cannot be used (a re-dispatch); when `false` that case raises the
`BufferTooSmallException` path in the proxy (HTTP 500 with the required size).

`vespera.bridge.max-buffered-request-bytes` defaults to 64 MiB (`0` =
unlimited), matching the Rust-side `VESPERA_MAX_REQUEST_BYTES` default.  The
proxy rejects SYNC/ASYNC/DIRECT/STREAMING requests whose buffered body exceeds
the cap with HTTP 413.  Bidirectional
streaming is exempt and remains the path for large or unknown-size uploads.

`vespera.bridge.max-buffered-response-bytes` (default 64 MiB, `0` = unlimited)
bounds the response bodies the heap-buffered `SYNC` / `ASYNC` paths hold. It is
**forwarded to the native buffered-response cap** during context startup (unless
`vespera.bridge.native.max-buffered-response-bytes`,
`-Dvespera.limits.maxBufferedResponseBytes` or
`VESPERA_MAX_BUFFERED_RESPONSE_BYTES` sets that cap explicitly), so Rust stops
collecting an oversized response before it is allocated on either heap and
answers **`500`** — a server-side limit on what the handler produced, not the
request-size status `413`. When the native cap could not be aligned (the
library dispatched before the context started, or a controller built by hand),
the proxy keeps a post-dispatch check with the same `500`. Use a streaming mode
for large downloads.

#### Proxied paths (include / exclude patterns)

By default the proxy is a `/**` catch-all: more specific Spring MVC handlers
(your `@RestController`s, `/error`, Actuator) still win, but everything else —
including what Spring Boot would otherwise serve from `classpath:/static/` —
goes to Rust. Narrow it with Spring
[`PathPattern`](https://docs.spring.io/spring-framework/reference/web/webmvc/mvc-controller/ann-requestmapping.html#mvc-ann-requestmapping-uri-templates)
patterns, matched against the path within the application (context path and a
prefix `DispatcherServlet` mapping removed):

```yaml
vespera:
  bridge:
    include-patterns: ["/api/**", "/health"]   # default ["/**"]
    exclude-patterns: ["/api/internal/**"]      # default []
```

A path outside `include-patterns`, or matching an `exclude-patterns` entry, is
**not mapped to the proxy at all**: Spring MVC carries on with its other handler
mappings — other controllers, static resources, and finally its own `404` —
exactly as if the proxy did not exist. With `cors=rust`, preflights for those
paths are left to Spring too. An invalid pattern fails startup. With the
defaults the proxy's `@RequestMapping("/**")` is left untouched; otherwise
`VesperaProxyPathMappings` re-registers it with the include patterns plus an
exclusion condition once the context's singletons are initialised.

#### IDE support

The JAR ships `META-INF/spring-configuration-metadata.json`, so IntelliJ IDEA
and the Spring Tools for VS Code / Eclipse complete and document every
`vespera.bridge.*` key (plus `management.health.vespera.enabled`) and offer the
values of `dispatch-mode`, `cors` and `springdoc.mode`.

### 2. Custom app-selection strategy

Resolve the app name however you like — URL path segment, subdomain, JWT claim, …

```java
@Bean
public AppNameResolver myAppResolver() {
    // Example: app name comes from the FIRST path segment
    //   /admin/dashboard  →  app "admin", path "/dashboard"
    //   /public/info      →  app "public", path "/info"
    return request -> {
        String uri = request.getRequestURI();
        if (uri.startsWith("/admin/")) return "admin";
        if (uri.startsWith("/public/")) return "public";
        return null;   // default app
    };
}
```

Spring autoconfigure's `@ConditionalOnMissingBean` automatically disables `HeaderAppNameResolver` when you supply your own bean — no further config needed.

### 3. Custom dispatch-mode policy

Pick `SYNC` for tiny JSON RPC, `BIDIRECTIONAL_STREAMING` for everything else:

```java
@Bean
public DispatchModeResolver myModeResolver() {
    return request -> {
        long contentLength = request.getContentLengthLong();
        if (contentLength >= 0 && contentLength < 4096
            && "application/json".equals(request.getContentType())) {
            return DispatchMode.SYNC;
        }
        return DispatchMode.BIDIRECTIONAL_STREAMING;
    };
}
```

### 4. BYO controller

Disable our controller entirely and route however you want.  The `VesperaBridge` native methods remain available for direct use:

```yaml
vespera:
  bridge:
    controller-enabled: false
```

```java
@RestController
public class MyController {
    @PostMapping("/api/admin/{path}")
    public ResponseEntity<?> adminRoute(@PathVariable String path, @RequestBody byte[] body) {
        byte[] wire = VesperaBridge.encodeRequest(
            "admin",                          // ← explicit app name
            "POST", "/" + path, null,
            Map.of("content-type", "application/json"),
            body);
        byte[] resp = VesperaBridge.dispatchBytes(wire);
        DecodedResponse d = VesperaBridge.decodeResponse(resp);
        return ResponseEntity.status(d.status()).body(d.bodyBytes());
    }
}
```

### 5. Custom async response executor

The ASYNC proxy path completes the native dispatch future from a Rust/Tokio
worker thread, then parses the wire response on a JVM-managed executor.  The
default bean is `vesperaBridgeAsyncResponseExecutor`, backed by
`ForkJoinPool.commonPool()`.  Replace that bean by name to use an application
executor:

```java
@Bean("vesperaBridgeAsyncResponseExecutor")
public Executor vesperaBridgeAsyncResponseExecutor() {
    return Executors.newFixedThreadPool(4);
}
```

## WebFlux

Spring WebFlux applications (reactor-netty, or any reactive server) get a non-blocking proxy instead of `VesperaProxyController`. Add the bridge to an app that has `spring-boot-starter-webflux`, call `VesperaBridge.init(...)` or set `vespera.bridge.library-name`, and that is all. `VesperaBridgeReactiveAutoConfiguration` activates only with `@ConditionalOnWebApplication(type = REACTIVE)` and only when `DispatcherHandler` and Reactor are on the classpath. The servlet autoconfiguration stays `SERVLET`-only, so a given app gets exactly one of the two proxies. WebFlux and Reactor are **compile-only** dependencies of the bridge, so they never reach a servlet or plain-Java consumer transitively. The build's `verifyPublishedPom` check fails if the published POM or Gradle module metadata declares any dependency.

```java
VesperaBridge.init("my_rust_lib");                          // or vespera.bridge.library-name
SpringApplication.run(ReactiveApplication.class, args);     // spring-boot-starter-webflux
```

**Autoconfigured beans** (each `@ConditionalOnMissingBean`, so any of them can be replaced):

| Bean | Default | Role |
|---|---|---|
| `ReactiveAppNameResolver` | `ReactiveHeaderAppNameResolver(vespera.bridge.app-header, vespera.bridge.exposed-apps)` | Picks the app per exchange. It uses the same allowlist and one-time WARN as the servlet `HeaderAppNameResolver`, and the selection header is never forwarded. |
| `ReactivePrincipalResolver` | `ExchangePrincipalResolver`, or `ReactivePrincipalResolver.none()` when `forward-principal=false` | Fills the trusted `principal` wire field from `exchange.getPrincipal()`. With Spring Security WebFlux, that is the `ReactiveSecurityContextHolder` authentication. Authorities are read reflectively, and anonymous tokens are skipped. |
| `VesperaReactiveProxyHandler` | (`controller-enabled=true`) | The `WebHandler` that dispatches to Rust. |
| `VesperaReactiveHandlerMapping` | order `LOWEST_PRECEDENCE - 10` | Maps `include-patterns` / `exclude-patterns`. `@Controller`s and `RouterFunction`s win; static resources come after. |
| `VesperaReactivePreflightFilter` | (`cors=rust`) | WebFlux answers CORS preflights inside `DispatcherHandler` without invoking a handler, so this `WebFilter` routes owned-path preflights to Rust. |

**Dispatch.** Requests go through `VesperaBridge.dispatchAsync`, so a Netty event loop never blocks:

- The request body is aggregated with `DataBufferUtils.join`, capped by `max-buffered-request-bytes`. A declared or actual excess answers `413`.
- The completion is published on `Schedulers.parallel()`, not on the Tokio worker that completed it.
- The response is written as one zero-copy buffer, capped by `max-buffered-response-bytes` (`500`).
- **Cancel-on-disconnect.** When the client goes away, the `Mono` is cancelled, which cancels the `CompletableFuture`, and the native task is dropped at its next poll. This is the same `cancel0` path as the servlet `ASYNC` mode. Observers see such a request with status `499`.

**Shared behaviour.** The following work exactly as on the servlet side:

- `HeaderPolicy` (hop-by-hop and `Connection`-nominated headers)
- the request id (`vespera.bridge.request-id.*`, put in the MDC around the dispatch call)
- `remote_addr` and `scheme`
- CORS ownership (`cors=spring|rust`)
- `DispatchObserver` beans, `MicrometerDispatchObserver` and the health indicator
- `vespera.bridge.native.*`, `library-name` and `shutdown-on-close`
- the `VesperaClient` bean

**Streaming.** A request whose `Accept` header asks for `text/event-stream` or NDJSON (the same rule as `SmartDispatchModeResolver`) is dispatched through the native header-first streaming symbol (`dispatchStreamingWithHeader`) on `Schedulers.boundedElastic()`. The bytes are bridged with `DataBufferUtils.outputStreamPublisher` (backpressured, 64 KiB chunks) and written with `writeAndFlushWith`, so every chunk Rust emits is flushed to the client. A disconnect ends the blocking writer, and the native side stops on the failed write. All other requests take the buffered path. A stream that fails after the Rust header (a panic or a body error, `VesperaStreamAbortedException`) is answered like the servlet proxy does: while nothing was committed, the Rust status and headers (`Set-Cookie`, `Cache-Control`, ...) are discarded and the proxy writes its own `500` (`panic in Rust engine` / `response body stream error`, `DispatchEvent.panicked` set for a panic); once bytes reached the client, the connection is aborted so the response never looks complete.

**Form bodies read by a `WebFilter`.** `exchange.getFormData()` consumes an `application/x-www-form-urlencoded` body (Spring Security's `CsrfWebFilter` calls it for every unsafe form request), and reactor-netty cannot replay it. When the body comes back empty or short, the proxy rebuilds it from the cached form data (re-encoded in the `Content-Type` charset, else UTF-8) and forwards the matching `Content-Length`. Values are grouped per name (`a=1&b=2&a=3` arrives as `a=1&a=3&b=2`).

**Properties that do not apply to WebFlux:** `dispatch-mode` (there is no `DispatchModeResolver`: the proxy always uses async dispatch, or streaming per `Accept`), `direct-retry-on-overflow`, `async-pool-size` and `clear-threadlocals-after-request`.

**Limitations:**

- Request bodies are always aggregated; there is no streamed upload equivalent of `BIDIRECTIONAL_STREAMING`.
- Each streamed response holds one `boundedElastic` thread for its lifetime, so size `reactor.schedulers.defaultBoundedElasticSize` for many concurrent SSE clients. An idle stream notices a disconnect only at its next write.
- `HEAD` is never streamed.
- A form body that a filter already consumed through `exchange.getFormData()` is not rebuilt (unlike the servlet proxy's parameter-map fallback).
- A path that a user `@Controller` owns for another HTTP method answers `405` from Spring rather than falling through to Rust.
- The springdoc integration is servlet-only.
- `VesperaClient.send()` blocks. From reactive code, use `sendAsync()` (`Mono.fromFuture(...)`).

## Multi-app routing

Register multiple named apps on the Rust side with `vespera::jni_apps!`:

```rust
// src/lib.rs of your cdylib crate
pub fn create_app()  -> axum::Router { vespera!(title = "Default") }
pub fn admin_app()   -> axum::Router { vespera!(dir = "admin_routes",  title = "Admin")  }
pub fn public_app()  -> axum::Router { vespera!(dir = "public_routes", title = "Public") }

vespera::jni_apps! {
    "_default" => create_app,
    "admin"    => admin_app,
    "public"   => public_app,
}
```

From the Java side, the default `HeaderAppNameResolver` selects an app per request:

```bash
# Default app (no header)
curl http://localhost:8080/health

# Admin app
curl -H "X-Vespera-App: admin" http://localhost:8080/dashboard

# Public app
curl -H "X-Vespera-App: public" http://localhost:8080/info
```

Each app's URLs are independent — the same `/users` path can mean different things in `admin` vs `public` apps.  Unknown app names return `404`; invalid app names (special characters, > 64 bytes) return `400`.

### Security: the app header is client-controlled

> **Set `vespera.bridge.exposed-apps`.** `X-Vespera-App` comes from the client.
> Without an allowlist **any client can route any URL into any registered app**:
> `GET /users` with `X-Vespera-App: admin` reaches the `admin` app's `/users`,
> sailing past a Spring Security rule that only protects `/admin/**` (path-based
> rules never see which app the request lands in).

```yaml
vespera:
  bridge:
    exposed-apps: [public]   # only these (+ the default app) are selectable
```

- With `exposed-apps` set, any other app name is answered with **`404` by the proxy itself — the request is never dispatched to Rust**. The default app (`_default`, or no header) is always selectable.
- Without it the old behaviour is kept, but the first request that selects a non-default app logs a one-time `WARN`.
- Apps that must not be reachable from the public internet should not be selectable at all: either leave them out of `exposed-apps` and reach them through a separate deployment, or let a **trusted gateway** set the header (and strip any client-supplied copy), or replace the resolver with one that derives the app from something the client cannot forge (path, host, authenticated principal) — a custom `AppNameResolver` bean may throw `UnknownAppException` to produce the same pre-dispatch `404`.
- The selection header is **not forwarded** to Rust (`AppNameResolver.selectionHeaderName()`), so handlers never see — or trust — it.

See [`examples/rust-jni-demo/`](../../examples/rust-jni-demo/) for a complete working example with two registered apps, including an admin dashboard route reachable only via the `admin` app.

## CORS

Spring MVC answers every CORS **preflight** (`OPTIONS` with `Origin` and `Access-Control-Request-Method`) itself, before any controller runs. Before 0.6 a preflight therefore never reached the Rust router: without a Spring CORS configuration Spring answered it with an empty `200` carrying no `Access-Control-Allow-Origin` (so the browser blocked the real request), and with one *and* a Rust `CorsLayer` the actual responses carried duplicate `Access-Control-Allow-Origin` headers (browsers reject those).

`vespera.bridge.cors` picks the owner:

| Value | Preflights | `Access-Control-*` on responses |
|---|---|---|
| `rust` (default) | Routed to the proxy by a highest-precedence handler mapping (`VesperaPreflightHandlerMapping`), so the Rust router answers them — configure a `tower_http::cors::CorsLayer` there | A Rust header replaces one Spring or a filter already set |
| `spring` | Answered by Spring MVC (`WebMvcConfigurer#addCorsMappings`, `@CrossOrigin`) or Spring Security `http.cors()` — the pre-0.6 behaviour | A Rust header is dropped when Spring already set it |

With `rust`, do not also configure Spring CORS for proxied paths, and do not enable Spring Security's `http.cors()` (its `CorsFilter` answers preflights before they reach Spring MVC). Without a CORS layer in Rust, a preflight gets whatever the router answers for `OPTIONS` (axum: `405` for a route without an `OPTIONS` handler).

## Spring Security

The proxy is an ordinary Spring MVC controller, so Spring Security's filter chain runs in front of it:

- **CSRF blocks proxied writes by default.** With Spring Security on the classpath, `CsrfFilter` rejects every `POST` / `PUT` / `PATCH` / `DELETE` without a CSRF token with `403` before the proxy runs. For a token-authenticated JSON API, disable CSRF for the proxied paths (`http.csrf(csrf -> csrf.ignoringRequestMatchers("/api/**"))`) or implement the token exchange; keep it for cookie-session browser forms.
- **Match on the path Rust sees.** Authorization rules match the servlet path. The proxy forwards the path *within the application* (context path and a prefix servlet mapping stripped, `;` path parameters dropped — see [Operability](#client-address-scheme-and-servlet-path)), so write rules against that path, not the raw request URI, and keep Spring's default `StrictHttpFirewall` (it rejects encoded `/`, `;`, `..` that could make a path look different to Spring and to Rust).
- **App selection bypasses path rules.** `X-Vespera-App` picks a whole router; restrict it with `vespera.bridge.exposed-apps` (see [Multi-app routing → Security](#security-the-app-header-is-client-controlled)) or with a custom `AppNameResolver` that checks the authenticated principal.
- **The authenticated principal is forwarded for you — don't use headers for it.** The proxy puts the authenticated user into the trusted `principal` wire field (see [Forwarded principal](#forwarded-principal)), which a client cannot set; Rust handlers take `vespera::inprocess::Principal`. Do not identify the user from a forwarded `X-User`-style header: request headers are client-controlled unless every copy is stripped.
- **Preflights.** With `vespera.bridge.cors=rust`, permit unauthenticated `OPTIONS` preflights (`.requestMatchers(HttpMethod.OPTIONS, "/**").permitAll()`), or browsers cannot reach the Rust CORS layer.

### Forwarded principal

The proxy asks the `PrincipalResolver` bean for the request's authenticated principal and sends it in the request wire header's `principal` field — a side channel only the bridge writes, never built from client headers. The default `ServletPrincipalResolver` forwards `HttpServletRequest.getUserPrincipal()` (set by the servlet container or by Spring Security's request wrapper) by name, plus — when Spring Security is on the classpath (detected reflectively; the bridge does not depend on it) — the `Authentication`'s `getAuthorities()`. Anonymous requests forward nothing. Forwarding is on by default: the value comes from server-side authentication, so it is safe to pass, and Rust handlers that do not ask for it ignore it.

```rust
use vespera::inprocess::Principal;

#[vespera::route(get)]
pub async fn me(principal: Principal) -> String {          // 401 when anonymous
    format!("{} admin={}", principal.name, principal.has_authority("ROLE_ADMIN"))
}
// Option<Principal> never rejects; Extension<Principal> also works (500 when absent).
```

- `vespera.bridge.forward-principal=false` forwards no principal.
- A custom resolver replaces the default, e.g. to add token claims as attributes:

  ```java
  @Bean
  PrincipalResolver principalResolver() {
      ServletPrincipalResolver servlet = new ServletPrincipalResolver();
      return request -> {
          VesperaPrincipal p = servlet.resolvePrincipal(request);
          return p == null ? null
                  : new VesperaPrincipal(p.name(), p.authorities(), Map.of("tenant", tenantOf(request)));
      };
  }
  ```

  Build it only from authentication state the server established, never from raw request headers.
- A hand-written `VesperaBridge.HeaderSource` forwards one by overriding `principal()`.

## springdoc-openapi (Swagger UI)

An application that uses [springdoc-openapi](https://springdoc.org/) for its Java controllers gets the Rust routes into the same Swagger UI. The integration (`VesperaSpringdocAutoConfiguration`) activates by itself when springdoc is on the classpath — springdoc stays **your** dependency (`org.springdoc:springdoc-openapi-starter-webmvc-ui`, 3.x for Boot 4, 2.8+ for Boot 3.5); the bridge only compiles against it.

**Requirement:** the Rust app serves its document — add `spec_url` to `vespera!`:

```rust
pub fn create_app() -> axum::Router {
    vespera!(spec_url = "/openapi.json", title = "My API", version = "1.0.0")
}
```

The bridge fetches it **in-process** (a native `GET /openapi.json` dispatch, no HTTP or socket), so it is always the document of the loaded cdylib, including any `merge = [...]`. A different route goes in `vespera.bridge.springdoc.spec-path` (per app: `spec-paths.<app>`); to read it from somewhere else (e.g. the `openapi.json` written by `vespera!(openapi = ...)`, bundled as a resource), register your own `VesperaOpenApiSource` bean.

Two modes (`vespera.bridge.springdoc.mode`):

| Mode | Result |
|---|---|
| `group` (default) | Every documented app becomes its own entry of the Swagger UI definition dropdown (`vespera`, `vespera (admin)`, …), served **verbatim** at `/v3/api-docs/vespera/<app>`. springdoc's own document stays as it is and is listed as `application` (selected first) unless you configured `springdoc.swagger-ui.urls` or springdoc groups — those are kept. |
| `merge` | The Rust paths, components and tags are merged into springdoc's default `/v3/api-docs` (an `OpenApiCustomizer`): paths outside `include-patterns` / inside `exclude-patterns` are dropped; a path springdoc already documents keeps the Java definition (logged); a different component under a clashing name is renamed `vespera_<Name>` (`<app>_<Name>` for a named app) with every `$ref` rewritten, an identical one is shared; a named app's operations get a required `X-Vespera-App` (`app-header`) parameter so "Try it out" reaches it; the Rust top-level `security` is copied onto its operations. springdoc caches the document, so initialise the native library before the first `/v3/api-docs` request. |

```yaml
vespera:
  bridge:
    springdoc:
      enabled: true                # false: no integration (the proxy stays hidden from springdoc)
      mode: group                  # group | merge
      spec-path: /openapi.json     # the Rust spec_url, dispatched in-process
      spec-paths:
        admin: /admin/openapi.json # per-app override
      apps: [_default, admin]      # default: _default + vespera.bridge.exposed-apps
      docs-path: /v3/api-docs/vespera # group mode: documents served at <docs-path>/<app>
      display-name: vespera        # group mode: Swagger UI label (named apps: "vespera (admin)")
```

- **Multi-app (`jni_apps!`):** every documented app gets its own definition (`group`) or is merged with the app header parameter (`merge`). Only `_default` and `vespera.bridge.exposed-apps` are documented by default; `<docs-path>/<app>` answers `404` for any other app, `503` while the native library is not loaded and `502` when the Rust route fails (e.g. no `spec_url` — the reason is logged).
- In `group` mode "Try it out" on a named app's definition does not send `X-Vespera-App` (the Rust document does not know about it); use `merge` mode, or a gateway that selects the app.
- The documents live under springdoc's `/v3/api-docs/**`, so an existing Spring Security rule for the API docs covers them.
- The proxy's catch-all `/**` mapping is hidden from springdoc's document whenever springdoc is present.
- springdoc's `springdoc.api-docs.enabled=false` disables the integration too.

## Binary wire format

Both request and response use the same length-prefixed layout:

```
bytes 0..4    : u32 BE = header_json byte length N
bytes 4..4+N  : UTF-8 JSON
                  (request)  { "v":1, "method", "path",
                               "query"?, "headers"?, "app"?,
                               "remote_addr"?, "scheme"?,
                               "principal"? }
                  (response) { "v":1, "status", "headers",
                               "metadata", "validation_errors"? }
bytes 4+N..   : raw body bytes (UTF-8 text or binary —
                no encoding applied)
```

- `"v":1` is the protocol version. Mismatched versions return a `400` wire response with an explanatory plain-text body.
- `"principal"` is `null` or `{"name": string, "authorities"?: [string], "attributes"?: {string: string}}` (at most 1024 authorities and 1024 attributes; otherwise `400`). The bridge encoder forwards only the first `VesperaPrincipal.MAX_ENTRIES` (1024) of each and logs a one-time warning when it drops the rest. The bridge sets it from the `PrincipalResolver` only — see [Forwarded principal](#forwarded-principal).
- Multi-valued response headers (e.g. `set-cookie`) render as JSON arrays so semantics are preserved — they're never comma-joined.
- All failure paths (malformed wire, Rust panic, no app registered) return a valid length-prefixed response with status `4xx` / `5xx`, so the decoder never has to special-case errors.
- **Header values use a Latin-1 mapping.** Servlet containers expose header bytes as ISO-8859-1 strings and write response header chars back the same way, so each byte travels as the JSON char `U+0000–U+00FF`. Rust maps a request value whose chars are all `≤ U+00FF` back to those bytes (a value with a wider char keeps its UTF-8 encoding), and renders every response value byte as one char — so a UTF-8 `Content-Disposition: attachment; filename="résumé.pdf"` reaches the client byte-for-byte (previously the whole value was dropped). ASCII values are unchanged.

## Dispatch modes

`VesperaBridge` exposes six `byte[]`-based native methods plus a
direct-buffer path — all sharing the same wire format, same registered
router, and same panic-safe `catch_unwind` discipline:

| Method | Mode | Java side return | Memory footprint |
|---|---|---|---|
| `dispatchBytes(byte[])` | sync | `byte[]` (header + body) | full body in memory |
| `dispatchAsync(CompletableFuture<byte[]>, byte[])` | async (`CompletableFuture`) | `void` (future completes) | full body in memory |
| `dispatchStreaming(byte[], OutputStream)` | sync, response-streaming | `byte[]` (header only) | chunk-bounded response |
| `dispatchFullStreaming(byte[], InputStream, OutputStream)` | sync, **bidirectional streaming** | `byte[]` (header only) | chunk-bounded both ways |
| `dispatchStreamingWithHeader(byte[], Consumer<byte[]>, OutputStream)` | sync, response-streaming | `void` (header via callback, fires before first body byte) | chunk-bounded response |
| `dispatchFullStreamingWithHeader(byte[], Consumer<byte[]>, InputStream, OutputStream)` | sync, bidirectional streaming | `void` (header via callback) | chunk-bounded both ways |
| `dispatchDirect(ByteBuffer, int, ByteBuffer)` | sync, **direct buffers** | `int` (response length / overflow code) | full body, but no Java heap arrays |

Pick the mode that matches your workload:
- Small JSON RPC, single request/response → `dispatchBytes`
- Hot small/bounded payloads where JNI copy overhead matters → `dispatchDirect` / `dispatchDirectPooled`
- Async I/O coordination (parallel Java requests, non-blocking) → `dispatchAsync` + `CompletableFuture`
- Large download / streaming response (video, PDF, server-sent events) → `dispatchStreaming` + `OutputStream`
- **Large upload + large download** (file transfer proxy, video transcoding, 1 GB ↔ 1 GB) → `dispatchFullStreaming` + `InputStream` + `OutputStream`
- The `*WithHeader` variants let Spring-style controllers commit status/headers from the callback **before** the first body byte is written

## Direct buffer dispatch (no JNI region copies)

`dispatchDirect(ByteBuffer in, int inLen, ByteBuffer out)` reads the
wire request from a **direct** `ByteBuffer` and writes the wire
response into another, eliminating the two JNI
`GetByteArrayRegion`/`SetByteArrayRegion` copies and the per-call Java
heap array allocations that `dispatchBytes` pays.  On the success path
the response is **streamed straight into the out buffer** (wire header
first, then each body frame at its final offset) — no intermediate
response `Vec`.  To be precise about what remains: one plain native
memcpy on the request side (axum requires owned request bytes) plus
the per-frame body copies; `422` responses are materialised internally
to keep `validation_errors` hoisted in the wire header.  Measured at
**1.4–3.4× per round-trip** versus `dispatchBytes` depending on
payload size.

Contract:
- Both buffers MUST be direct (`ByteBuffer.allocateDirect`); heap
  buffers are rejected with `IllegalArgumentException` before crossing
  JNI.
- The request is read from absolute offsets `in[0..inLen]` — the
  buffer's position/limit are **ignored**; `inLen` is authoritative.
- Return `>= 0`: a complete wire response occupies `out[0..n]`.
- Return `< 0`: `-(requiredSize)` — the response did not fit; buffer
  contents are undefined (a prefix may have been written).
  `requiredSize` is exact, but **retrying the raw `dispatchDirect`
  re-runs the Rust handler**, so only retry safe requests.  (The pooled
  API below does not re-run: it fetches the stashed response.)
- `Integer.MIN_VALUE`: response exceeds 2 GiB (unrepresentable).

`dispatchDirectPooled(byte[] wireRequest, boolean retryOnOverflow)`
wraps the raw call with per-thread reusable direct buffers (64 KiB
initial, doubling up to the `vespera.direct.maxBufferBytes` system
property, default 4 MiB) and returns a read-only view of the response
valid until the next dispatch on the same thread.  On response
overflow the finished response stays in a native stash (at most 1024
entries, 30 s TTL) and is copied into a grown pooled buffer — **the
handler is not re-run**, whatever `retryOnOverflow` says.  Only a
response beyond the pool cap or a `text/event-stream` response throws
`BufferTooSmallException(requiredSize)` (the stash entry is released);
the Spring proxy streams those from the stash instead.

The fastest variant skips the intermediate wire `byte[]` entirely —
`dispatchDirectPooled(appName, method, path, query, headers, body,
retryOnOverflow)` encodes straight into the pooled direct buffer via
`encodeRequestInto(...)`, so the body is copied heap→direct exactly
once.  `encodeRequestInto(..., ByteBuffer target)` is also public for
callers managing their own buffers; it returns the bytes written or
`-(required)` without touching the buffer when `target` is too small
(an encoding-side signal — no dispatch has run, growing and retrying
is always safe, unlike the response-overflow retry).

For the Spring proxy, `SmartDispatchModeResolver` is the
**autoconfigured default since 0.2.0** — `DispatchMode.DIRECT` /
`SYNC` activate automatically on small bounded requests, no property
required.  Restore the pre-0.2.0 default (every request that may carry
a body streams both ways) with:

```yaml
vespera:
  bridge:
    dispatch-mode: bidirectional-streaming   # default since 0.2.0: smart
```

`smart` picks the cheapest safe path per request (measured on a small
`GET /health` round-trip through the real JNI boundary):

| Request shape | Mode | ns/round-trip |
|---|---|---|
| `Accept: text/event-stream` / `application/x-ndjson` / `application/stream+json` | `STREAMING` (body ≤ 256 KiB) | — |
| Small/bodyless + safe (GET/HEAD/OPTIONS) | `DIRECT` | ~2,200 |
| Small (≤ 256 KiB Content-Length) + unsafe (POST/PUT/PATCH/DELETE) | `SYNC` | ~3,200 |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | ~24,100 |

A response that overflows the pooled buffer is fetched from the native
overflow stash (or streamed from it beyond the 4 MiB cap / for event
streams), so DIRECT no longer re-runs the handler; the safe-method gate
is kept for the rare re-dispatch fallback.  SYNC never re-runs the handler (safe for POST), but
buffers the full response on the JVM heap, which the request-size gate
keeps reasonable for JSON-RPC-shaped traffic.

Custom policies can still register the bean directly (the property is
ignored when a user `DispatchModeResolver` bean exists):

```java
@Bean
public DispatchModeResolver dispatchModeResolver() {
    return new BidirectionalStreamingDispatchModeResolver();
}
```

### Virtual thread (Project Loom) limitation

The pooled direct-buffer methods (`dispatchDirectPooled`) use
`ThreadLocal<ByteBuffer[]>` to maintain per-thread reusable buffers
(64 KiB initial, growing to `vespera.direct.maxBufferBytes`, default
4 MiB).  In Java 21+, `ThreadLocal` binds to the **virtual thread**
(not the carrier thread) — so on a virtual thread each dispatch would
allocate a fresh direct buffer, lose all pooling benefit, and
accumulate off-heap memory until the virtual thread is
garbage-collected.

**Automatic mitigation (since 0.2.1):** `dispatchDirectPooled` detects
the calling thread via `Thread.isVirtual()` (resolved reflectively so
the library still targets Java 17) and, when it is a virtual thread,
**routes the request to the GC-managed heap `dispatchBytes` path
instead of the pooled direct buffer** — no per-vthread off-heap
accumulation, no configuration required.  The DIRECT fast path keeps
its pooling benefit on platform threads (Tomcat's default request
pool); virtual-thread deployments transparently fall back to the heap
path at a small per-call allocation cost.

**Carrier pinning:** a virtual thread blocked inside a native call is
pinned to its carrier.  On a virtual thread the proxy's `SYNC` path (the
heap fallback above, and `VesperaClient.send()`) therefore never blocks
natively across a wait.  Since 0.6 it is **poll-first**: the Rust dispatch
is polled once on the calling thread; a handler that completes without
waiting answers right there (pinned-call cost, ~1.3 µs p50), and one that
has to wait (I/O, timers) is handed to a Tokio worker while the virtual
thread parks on a `CompletableFuture` — the carrier is released for the
wait.  `-Dvespera.virtualThread.pollFirst=false` restores the pre-0.6
`dispatchAsync` + `join()` for every request (~15 µs p50 and ~1/5 the
throughput for a handler that never waits).  Both apply the
`vespera.async.maxInFlight` cap.  Measurements:
[`docs/virtual-thread-park.md`](docs/virtual-thread-park.md).  Platform
threads keep the direct call.  The streaming modes still pin (they write to
the servlet streams from the calling thread).

You can still opt out of DIRECT entirely if you prefer streaming
end-to-end:
- Set `vespera.bridge.dispatch-mode=bidirectional-streaming` so DIRECT
  is never chosen by the autoconfigured resolver.
- Or use `dispatchBytes`, `dispatchStreaming`, or
  `dispatchFullStreaming` directly.
- Or lower `vespera.direct.maxBufferBytes` to reduce per-thread
  allocation size on platform threads.

`DispatchMode.BIDIRECTIONAL_STREAMING` is safe for virtual threads
and handles all payload sizes without pooling.

### Synchronous dispatch runtime semantics (background tasks)

Every dispatch mode runs on the **same shared multi-threaded Tokio
runtime**. `SYNC` and `DIRECT` `block_on` it from the calling Java thread,
so the request future runs on that thread while I/O, timers and any
`tokio::spawn`ed task use the shared drivers and workers. Connection pools
(sqlx / sea-orm, reqwest, hyper) created in one request keep working from
every other request and mode, and fire-and-forget `tokio::spawn` tasks make
progress on their own.

Worker count: `vespera.runtime.workerThreads` / `VESPERA_RUNTIME_WORKERS`;
blocking pool: `vespera.runtime.maxBlockingThreads` /
`VESPERA_RUNTIME_MAX_BLOCKING_THREADS` (default 512).

> Up to 0.5.x, `SYNC`/`DIRECT` used a per-Java-thread
> current-thread runtime; runtime-bound resources shared across requests
> could stall or fail with "runtime is shutting down".

### Operational sizing (threads & off-heap)

All dispatch modes share **one** process-wide multi-threaded Tokio runtime;
`SYNC`/`DIRECT` no longer create a runtime per Java request thread, so the
thread footprint no longer scales with the servlet pool size. On a large
servlet pool (e.g. 200 Tomcat threads) the steady-state cost is:

- **Off-heap**: the `DIRECT` fast path keeps per-platform-thread pooled
  direct `ByteBuffer`s (64 KiB → `vespera.direct.maxBufferBytes`, default
  4 MiB), so up to `poolThreads × maxBufferBytes` retained direct memory
  once each thread has served a large response (adaptive shrink reclaims it
  after idle dispatches). Lower `vespera.direct.maxBufferBytes` if off-heap
  pressure matters more than the DIRECT copy savings.
- **Runtime workers**: one fixed set of async workers for the whole process
  (default: logical CPUs). `SYNC`/`DIRECT` drive their request future on the
  calling Java thread (`block_on`), so they add no worker threads. Cap the
  workers with `vespera.runtime.workerThreads` /
  `VESPERA_RUNTIME_WORKERS` when the JVM's own pools compete for the same
  cores, or when a container CPU limit is below the host CPU count.
- **Blocking threads**: `spawn_blocking` work (the multipart temp-file
  extractor, request-body pulls on the bidirectional streaming paths, user
  handlers) shares a single blocking pool capped at
  `vespera.runtime.maxBlockingThreads` /
  `VESPERA_RUNTIME_MAX_BLOCKING_THREADS` (default 512). The cap is
  process-wide, independent of the servlet pool; excess blocking work queues
  instead of spawning more threads. Idle blocking threads are reaped by
  Tokio after a keep-alive period.

## Programmatic client (`VesperaClient`)

To call the embedded Rust app from Java code — a Spring service, a
`@Scheduled` job, a message listener — without HTTP and without the proxy
controller, use `VesperaClient`. It builds the wire request with the same
codec as the proxy, dispatches it in-process and returns a `VesperaResponse`:

```java
VesperaClient client = VesperaClient.create();            // default app
VesperaResponse r = client.get("/users/42")
        .header("Accept", "application/json")
        .query("expand", "roles teams")                    // -> ?expand=roles%20teams
        .send();
r.status();                 // 200
r.header("content-type");   // case-insensitive, first value
r.bodyString();             // charset from content-type, else UTF-8
r.bodyBytes();              // owned copy (r.body() = read-only ByteBuffer view)
r.validationErrors();       // hoisted 422 errors, [] otherwise

client.post("/users").json("{\"name\":\"a\"}").send();     // raw JSON string/bytes
client.post("/upload").body(bytes, "application/octet-stream").send();
client.patch("/profile").form(Map.of("name", "Jane Doe")).send();
client.forApp("admin").get("/dashboard").send();          // named app (jni_apps!)

CompletableFuture<VesperaResponse> f = client.get("/x").sendAsync();
```

| Call | Dispatch path |
|---|---|
| `send()` | SYNC `dispatchBytes`; on a virtual thread it polls the dispatch once inline and parks on a Tokio-completed future only when the handler waits (never pins the carrier across a wait) |
| `sendAsync()` | ASYNC `dispatchAsync` (counts toward `vespera.async.maxInFlight` → `503`). The future completes on a Tokio worker: use `*Async` continuations for heavy work, or `sendAsync(executor)` |
| `body(InputStream, contentType)` + `send()` | bidirectional streaming: Rust pulls the request body chunk-by-chunk (capped by `maxStreamingRequestBytes`); the response is collected in memory. Sent once; async only via `sendAsync(executor)` |

- **No JSON mapping.** Bodies are raw bytes or strings; the bridge never
  parses domain JSON. Use your own mapper on `bodyString()` / `bodyBytes()`.
- **Statuses are responses, not exceptions.** `4xx` / `5xx`, the `503` of a
  shut-down runtime and the `500` of a Rust panic (`r.panicked()`) are
  returned normally. Only transport failures — library not initialised,
  missing native symbol, malformed wire, a streamed body failing mid-transfer —
  throw the unchecked `VesperaClientException` (or fail the future with it).
- Header names are lower-cased; `header()` replaces, `addHeader()` joins with
  `", "` (`"; "` for `cookie`). Query names/values are percent-encoded
  (RFC 3986, UTF-8); a `?query` already in the path is kept and appended to.
  `VesperaClient.encodePathSegment(id)` encodes path segments.
- `remoteAddr("ip:port")` / `scheme("https")` fill the optional
  `remote_addr` / `scheme` wire fields (`ConnectInfo<SocketAddr>` / `Scheme`).
- `principal(VesperaPrincipal.of("alice", "ROLE_USER"))` fills the trusted
  `principal` wire field (Rust: `vespera::inprocess::Principal`); the client
  never derives it from headers.
- **Spring:** the autoconfiguration exposes a `VesperaClient` bean (default
  app; `@ConditionalOnMissingBean`), also when
  `vespera.bridge.controller-enabled=false`. It is created after
  `vespera.bridge.native.*` / `library-name` are applied.

```java
@Service
class ReportJob {
    private final VesperaClient vespera;
    ReportJob(VesperaClient vespera) { this.vespera = vespera; }

    @Scheduled(cron = "0 0 * * * *")
    void run() {
        VesperaResponse r = vespera.post("/reports/rebuild").json("{}").send();
        if (!r.isSuccessful()) log.warn("rebuild failed: {} {}", r.status(), r.bodyString());
    }
}
```

### Kotlin

The same JAR carries Kotlin extensions for the client (`VesperaKotlin.kt`,
package `com.devfive.vespera.bridge`) — no extra artifact. `kotlin-stdlib`
and `kotlinx-coroutines-core` are **not** dependencies of the bridge: a Kotlin
app already has the stdlib, and `sendAwait` needs
`org.jetbrains.kotlinx:kotlinx-coroutines-core` (>= 1.7) on the classpath.
Compiled with Kotlin language/API version 2.0, so Kotlin 1.9+ compilers
(Spring Boot 3.5) can consume it.

```kotlin
import com.devfive.vespera.bridge.VesperaClient
import com.devfive.vespera.bridge.get
import com.devfive.vespera.bridge.post
import com.devfive.vespera.bridge.query
import com.devfive.vespera.bridge.sendAwait

@Service
class UserService(private val vespera: VesperaClient) {
    suspend fun user(id: String): String? {
        val r = vespera.get("/users/${VesperaClient.encodePathSegment(id)}") {
            header("accept", "application/json")
            query("expand" to "roles", "flag" to null)   // -> ?expand=roles&flag
        }.sendAwait()                                     // suspends, never blocks
        return if (r.isSuccessful) r.bodyString() else null
    }

    suspend fun create(body: String) = vespera.post("/users") { json(body) }.sendAwait()
}
```

| Extension | What it does |
|---|---|
| `VesperaRequest.sendAwait()` | `sendAsync().await()`: suspends until the ASYNC dispatch completes. **Cancelling the coroutine** (`withTimeout`, a cancelled scope, a client disconnect in a coroutine controller) cancels the `CompletableFuture` and thereby the native dispatch (cancel-on-disconnect: the Rust handler is dropped, its in-flight slot released). Transport failures throw `VesperaClientException`; statuses are responses |
| `VesperaRequest.sendAwait(executor)` | `sendAsync(executor).await()` — required for an `InputStream` body (the blocking streamed upload runs on `executor`) |
| `client.get(path) { ... }` (`head` / `post` / `put` / `patch` / `delete` / `options` / `request(method, path)`) | builds the request with a `VesperaRequest.() -> Unit` block (`header`, `query`, `json`, `app`, `principal`, ...); returns the request for `send()` / `sendAwait()` |
| `query("a" to 1, "b" to null)` / `headers("x" to "y")` | vararg `Pair` helpers |

The Java client API carries [JSpecify](https://jspecify.dev) nullability
annotations (`@NullMarked` + `@Nullable`, compile-only), so Kotlin sees
`response.header("x")` / `contentType()` / `appName()` as `String?` and
everything else as non-null instead of platform types.

The DIRECT pooled-buffer path stays a proxy optimisation (its per-thread
buffer view would have to be copied out anyway). For full control (header-first
streaming into your own `OutputStream`, `dispatchDirect`), use the low-level
API below.

## Direct API (without the proxy controller)

For custom integrations bypassing Spring:

```java
import com.devfive.vespera.bridge.VesperaBridge;
import com.devfive.vespera.bridge.VesperaBridge.DecodedResponse;

// 1. Initialise once at startup
VesperaBridge.init("my_rust_lib");

// 2. Encode a request
byte[] wireRequest = VesperaBridge.encodeRequest(
    "POST",
    "/documents/validate",
    /* query */ null,
    Map.of("content-type", "application/json"),
    "{\"title\":\"…\"}".getBytes(StandardCharsets.UTF_8));

// 3. Dispatch through Rust
byte[] wireResponse = VesperaBridge.dispatchBytes(wireRequest);

// 4. Decode
DecodedResponse resp = VesperaBridge.decodeResponse(wireResponse);
System.out.println(resp.status());           // 200
System.out.println(resp.headers());          // { "content-type": "application/json", … }
System.out.println(new String(resp.bodyBytes())); // copies the raw response body
```

### Async dispatch (`CompletableFuture`)

```java
import java.util.concurrent.CompletableFuture;

byte[] wireRequest = VesperaBridge.encodeRequest(
    "POST", "/documents/validate", null,
    Map.of("content-type", "application/json"),
    body);

CompletableFuture<byte[]> future = VesperaBridge.dispatch(wireRequest);
// Non-blocking — the calling thread continues; the future is
// completed from a Tokio worker thread.

future.thenAccept(wireResponse -> {
    DecodedResponse resp = VesperaBridge.decodeResponse(wireResponse);
    System.out.println("Status: " + resp.status());
});

// Or block synchronously:
byte[] wireResponse = future.get();
```

Always-complete contract: the future is **always** completed with a
valid wire response, even on Rust panics or JNI conversion failures.
You will never see a dangling future.

**Cancellation propagates to Rust.** When the future completes
exceptionally — `future.cancel(..)`, `future.orTimeout(..)`, or your own
`completeExceptionally(..)` — the in-flight Rust handler is dropped at its
next `.await` (its DB queries, timers and other owned work stop) and its
`vespera.async.maxInFlight` slot is released at once. A normal completion
never cancels anything, and a cancel that races the completion is harmless.
The proxy's `ASYNC` mode cancels the native future when the servlet async
request ends first (Spring async timeout, container-reported error or client
disconnect).

#### Client disconnects per dispatch mode

| Mode | What happens when the HTTP client goes away |
|---|---|
| `ASYNC` (`dispatchAsync`) | The native future is cancelled on async timeout / container error → the Rust handler is dropped and its in-flight slot freed. |
| `STREAMING` / `BIDIRECTIONAL_STREAMING` (all streaming natives) | The next servlet write or flush fails → Rust stops polling and drops the response body immediately (the body is flushed before it waits, so a slow producer is detected at its first pause). A handler that has not produced its response header yet is not interrupted until it does. |
| `SYNC` / `DIRECT` | Not cancellable: the handler runs on `block_on` and the disconnect is only visible when the finished response is written. Bound these with `vespera.dispatch.timeoutMillis` / `vespera.bridge.native.dispatch-timeout`. |

### Streaming dispatch (large bodies, file uploads/downloads)

```java
import java.io.ByteArrayOutputStream;

byte[] wireRequest = VesperaBridge.encodeRequest(
    "GET", "/files/large.pdf", null, Map.of(), new byte[0]);

try (ByteArrayOutputStream sink = new ByteArrayOutputStream()) {
    // Body bytes stream into `sink` chunk-by-chunk during the call.
    byte[] headerOnly = VesperaBridge.dispatchStreaming(wireRequest, sink);

    DecodedResponse meta = VesperaBridge.decodeResponse(headerOnly);
    System.out.println("Status: " + meta.status());
    System.out.println("Body size: " + sink.size());
}
```

### Bidirectional streaming (large upload + large download)

```java
import java.io.InputStream;
import java.io.OutputStream;

// Request body comes from anywhere — file, socket, HTTP stream:
try (InputStream upload = Files.newInputStream(Path.of("huge.mp4"));
     OutputStream download = Files.newOutputStream(Path.of("transcoded.mp4"))) {

    byte[] wireHeader = VesperaBridge.encodeRequestHeader(
        "POST", "/transcode", null,
        Map.of("content-type", "video/mp4"));

    byte[] respHeader = VesperaBridge.dispatchFullStreaming(
        wireHeader, upload, download);

    DecodedResponse meta = VesperaBridge.decodeResponse(respHeader);
    System.out.println("Status: " + meta.status());
    // download already contains the transcoded video.
}
```

Memory characteristics: **roughly a 256 KiB chunk buffer + a 16-slot
mpsc channel buffer** in Rust (both configurable, see below), plus
normal JVM `byte[]` chunks. A 1 GiB upload paired with a 1 GiB
download runs in low-single-digit MiB resident memory on each side.
Backpressure is enforced naturally — if axum reads slowly,
`InputStream.read()` blocks on the bounded channel.

#### Streaming tuning

Both knobs are fixed for the process lifetime once the first dispatch
runs. Configuration precedence (first hit wins, then cached):

1. **Programmatic setter** — `VesperaBridge.configureStreaming(chunkBytes, channelCapacity)` (Java API, call before or after init; returns `void` for 0.5.x binary compatibility — a value the native side already fixed is logged as a warning)
2. **System properties** — `vespera.streaming.chunkBytes`, `vespera.streaming.channelCapacity`
3. **Environment variables** — `VESPERA_STREAMING_CHUNK_BYTES`, `VESPERA_STREAMING_CHANNEL_CAPACITY`
4. **Built-in defaults** — 256 KiB chunk size, 16 channel slots

| Setting | System property | Env var (fallback) | Default | Range |
|---|---|---|---|---|
| Chunk buffer size | `vespera.streaming.chunkBytes` | `VESPERA_STREAMING_CHUNK_BYTES` | 256 KiB | 4 KiB – 8 MiB |
| Request channel slots | `vespera.streaming.channelCapacity` | `VESPERA_STREAMING_CHANNEL_CAPACITY` | 16 | 1 – 1024 |
| Tokio worker threads | `vespera.runtime.workerThreads` | `VESPERA_RUNTIME_WORKERS` | logical CPUs | 1 – 1024 |

**Java API** — call before `VesperaBridge.init(...)` for guaranteed precedence:

```java
// Configure streaming parameters before init
VesperaBridge.configureStreaming(
    131072,  // chunkBytes: 128 KiB (clamped to 4 KiB – 8 MiB)
    32       // channelCapacity: 32 slots (clamped to 1 – 1024)
);
VesperaBridge.init("my_rust_lib");
```

When called before `init()`, values are stored as pending and applied
immediately after the native library loads, **before any dispatch can
occur**. This ensures the programmatic setter beats system properties
and environment variables (Rust-side precedence: setter > env > default).

When called after `init()`, the native library is already loaded and
values are applied immediately (still beats env vars, but system
properties may have already been read during init).

Throws `IllegalArgumentException` if `chunkBytes` is outside [4096, 8388608] or
`channelCapacity` is outside [1, 1024].

**System properties** — set before `VesperaBridge.init(...)`:

```bash
java -Dvespera.streaming.chunkBytes=131072 \
     -Dvespera.streaming.channelCapacity=32 \
     -jar app.jar
```

**Environment variables** — fallback when no system property is set:

```bash
export VESPERA_STREAMING_CHUNK_BYTES=131072
export VESPERA_STREAMING_CHANNEL_CAPACITY=32
java -jar app.jar
```

The worker-thread knob caps Rust's shared Tokio runtime — useful when
the JVM's own pools (Tomcat request threads, virtual-thread carriers)
compete with Tokio for the same cores, or when a container CPU limit
is lower than the host's logical CPU count.

Larger chunks reduce the per-chunk JNI crossing cost (one
`SetByteArrayRegion` + one `OutputStream.write` per chunk) at the
price of per-stream memory — 256 KiB is a reasonable ceiling for
throughput-oriented deployments.

### Server-side response streaming (Spring `StreamingResponseBody`)

Pair `dispatchStreaming` with `StreamingResponseBody` for true
server-side streaming — the JVM and Rust both process chunks
without ever holding the full body in memory:

```java
@GetMapping("/download/{name}")
public ResponseEntity<StreamingResponseBody> download(@PathVariable String name) {
    byte[] wireReq = VesperaBridge.encodeRequest(
        "GET", "/files/" + name, null, Map.of(), new byte[0]);

    // We need status/headers before Spring commits the response —
    // call streaming once with a buffered sink to get the header,
    // then stream the actual response. (For pure pass-through, use
    // a custom controller that wires Spring's HttpServletResponse
    // OutputStream directly.)
    StreamingResponseBody body = output -> {
        VesperaBridge.dispatchStreaming(wireReq, output);
    };
    return ResponseEntity.ok()
        .contentType(MediaType.APPLICATION_OCTET_STREAM)
        .body(body);
}
```

### Binary upload / download

The wire format carries bytes verbatim — no base64, no transcoding. A multipart file upload reaches the Rust `axum::extract::Multipart` extractor byte-for-byte:

```java
byte[] pdf = Files.readAllBytes(Path.of("report.pdf"));
byte[] wire = VesperaBridge.encodeRequest(
    "POST", "/upload", null,
    Map.of("content-type", "application/octet-stream"),
    pdf);
DecodedResponse resp = VesperaBridge.decodeResponse(
    VesperaBridge.dispatchBytes(wire));
assert Arrays.equals(pdf, resp.bodyBytes()); // exact round-trip (copy on demand)
```

A Rust handler returning a binary response (e.g. `image/png`) flows the same way: `VesperaProxyController` returns `ResponseEntity<byte[]>` for **every** content type — the wire header already carries the exact `Content-Type`, which Spring's `ByteArrayHttpMessageConverter` writes verbatim. (Before 0.2.1 text-like content types were delivered as `ResponseEntity<String>`; that path was dropped because it forced a redundant UTF-8 decode→re-encode round-trip.)

## VesperaProxyController behaviour

`@RequestMapping("/**")` catches every HTTP request, regardless of method or content type, and:

1. Collects all incoming headers (lowercased keys), minus hop-by-hop headers and the app-selection header.
2. Asks the configured `DispatchModeResolver` which mode serves this request (default since 0.2.0: `SmartDispatchModeResolver` — DIRECT for small/bodyless safe requests, SYNC for small unsafe requests, BIDIRECTIONAL_STREAMING for everything else; opt out with `vespera.bridge.dispatch-mode=bidirectional-streaming`).
3. For `SYNC` / `ASYNC` / `STREAMING` / `DIRECT` modes the body is read into `byte[]` first (bodyless requests — explicit `Content-Length: 0`, e.g. the small safe GETs the SmartDispatch resolver routes through DIRECT — skip the read and reuse a shared empty array), then encoded via `VesperaBridge.encodeRequest(...)` and dispatched through the matching native method.
   - **Form bodies consumed by Spring.** Spring's `FormContentFilter` (PUT/PATCH/DELETE) and any earlier `getParameter()` call (e.g. Spring Security's `CsrfFilter`, POST) drain an `application/x-www-form-urlencoded` body into request parameters before the proxy runs. When the stream comes back empty/short, the proxy rebuilds the body from `getParameterMap()` minus the query-string parameters (re-encoded in the charset it was decoded with) and forwards the matching `Content-Length`, so a Rust `Form<T>` extractor still sees every field. Form requests a resolver routes to `BIDIRECTIONAL_STREAMING` are served as `STREAMING` (request buffered, response streamed) for the same reason. `FormContentFilter` itself is left enabled for the host app. Values are grouped per name (`a=1&b=2&a=3` arrives as `a=1&a=3&b=2`).
4. Sync/async responses are parsed straight from the wire response via the allocation-lean `WireHeaderReader` (status + headers) and returned as `ResponseEntity<byte[]>` for **every** `Content-Type` — the body is sliced once from the wire tail; the `Content-Type` header is carried verbatim, so no text/binary branching is needed.  Streaming and DIRECT modes write status/headers and body straight to the servlet response.
5. `BIDIRECTIONAL_STREAMING` hands Rust a gated wrapper of the servlet `InputStream`. A handler that returns without draining the request body can leave the Rust request producer parked in `read()` (it is detached after a bounded join); once the native call returns, the gate makes every further read return `-1` without touching the servlet stream, and if a read was still in flight the proxy sets `Connection: close` (when the response is not committed yet; otherwise best effort — the container sees that read finish on its own) so a keep-alive connection is never reused at an unknown stream position.

## Operability

### Client address, scheme and servlet path

The proxy forwards the servlet request's remote address (`ip:port`, IPv6 bracketed) and scheme (`http` / `https`) as the optional wire fields `remote_addr` / `scheme`. Rust handlers read them with `ConnectInfo<SocketAddr>` and `Extension<http::uri::Scheme>` exactly like under `axum::serve` (configure Tomcat `RemoteIpValve` / `server.forward-headers-strategy` so they reflect the real client behind a load balancer). The forwarded path is application-relative: the context path and a prefix servlet mapping (`spring.mvc.servlet.path=/v1` → `/v1/users` arrives as `/users`) are stripped, and `;` path parameters (`;jsessionid=...`) are dropped from every segment.

### Request id

```yaml
vespera.bridge.request-id:
  enabled: true          # default
  header: X-Request-Id   # default
```

An incoming id (1–128 visible ASCII characters) is reused, otherwise a random UUID is generated. It is forwarded to Rust in the same header, echoed on the response (unless the Rust response already sets that header), put in the SLF4J MDC as `requestId` while the request is dispatched, and included in WARN logs and `DispatchEvent`s.

### Errors written by the proxy

Errors the proxy produces itself (unknown app 404, buffered body too large 413, buffered response too large 500, async backpressure 503, native library not initialized 503, DIRECT overflow, unexpected failures 500) are written directly as `text/plain; charset=utf-8` with the message as body, the same shape as the Rust `error_wire` responses, instead of going through Spring's `/error` page (whose shape depends on `Accept`). An unexpected exception is answered with the fixed body `vespera dispatch failed` — its message and stack trace go only to the WARN log, tagged with the request id the response echoes — so internals (hosts, paths, SQL) never reach clients. Writing such an error discards the buffered body and only the headers the proxy itself set; headers servlet filters set before the proxy ran (CORS, security headers, cookies) are kept.

If the native library was never loaded (`VesperaBridge.init` not called, or it failed), every proxied request is answered `503 vespera native library is not initialized` instead of escaping as an `UnsatisfiedLinkError` (whitelabel 500), and a WARN is logged once at startup.

### Health indicator

With Spring Boot Actuator on the classpath (Boot 4 `spring-boot-health` or Boot 3.5 `spring-boot-actuator` — both optional dependencies of the bridge), the `vespera` health indicator is registered: `UP` once the native library is loaded, `DOWN` (`reason: native library not initialized`) before that, `OUT_OF_SERVICE` after `VesperaBridge.shutdown(...)`. Its details carry `bridgeVersion` and the verified `nativeVersion`. Disable it with `management.health.vespera.enabled=false`; see it at `/actuator/health/vespera` (with `management.endpoint.health.show-details`).

### Panics, 5xx and metrics

A Rust panic answers `500` with `"panicked":"true"` in the response wire `metadata` (`DecodedResponse.metadata().get("panicked")`). Every dispatch that ends in a panic or a `5xx` is logged at WARN with method, path, app, mode, status, latency and request id. For metrics or tracing, register `DispatchObserver` beans; each is called once per request with a `DispatchEvent(app, mode, method, path, status, durationNanos, panicked, requestId)` (observer exceptions are logged and swallowed; observers and the WARN log run with the request id in the MDC, also for `ASYNC` completions on the response executor). When Micrometer is on the classpath and a `MeterRegistry` bean exists (e.g. Spring Boot Actuator), a `MicrometerDispatchObserver` is autoconfigured: timer `vespera.bridge.dispatch` tagged `app`, `mode`, `status` and `outcome` (`SUCCESS`, `CLIENT_ERROR`, `SERVER_ERROR`, `PANIC`, ...); timers are cached per tag combination. Micrometer stays an optional dependency.

The app name is client-controlled (`X-Vespera-App`), so it is **never reported verbatim**: `DispatchEvent.app()`, the `app` tag and the WARN log carry `_default`, an app listed in `vespera.bridge.exposed-apps`, an app Rust has answered from one of its handlers before (a status below 400, or a 4xx other than 400/404/408/413/431, or a panic — answers Rust's app resolution never produces; at most 256 learned names), or `UNKNOWN` (names rejected by the allowlist, invalid names, unregistered names). Random header values therefore cannot create unbounded metric series. An `ASYNC` request that hits Spring's async timeout is reported once with status `503` (what the client received); the late native result is discarded.

### Redeploy, class loaders and shutdown

```yaml
vespera.bridge:
  library-name: my_rust_lib   # VesperaBridge.init(...) during context startup (if not already loaded)
  shutdown-on-close: false    # default; true = stop the Rust runtime when the context closes
  shutdown-timeout: 10s
```

- A JVM can load a native library through **one class loader only**, and it is never unloaded. Loading it again from a redeployed web app fails with `UnsatisfiedLinkError: ... already loaded in another classloader`; the bridge rethrows it with guidance. On servlet containers that redeploy or host several apps, put `vespera-bridge` and the library in the container's shared lib (e.g. Tomcat `$CATALINA_BASE/lib`) or run one deployment per JVM, and restart the JVM to pick up a new native build.
- `VesperaBridge.shutdown(Duration)` stops the shared Tokio runtime and returns `false` if it was already shut down (or the library is not loaded). New dispatches answer `503` at once; in-flight dispatches of every mode are drained for up to the timeout, then the runtime stops — an `ASYNC` dispatch still pending completes its future with a `503` wire, and cron jobs stop. Called from a Tokio worker (an inline `CompletableFuture` continuation) it returns immediately and finishes in the background. It is one-way: the runtime cannot be restarted in the same process. Registered apps and the loaded library stay in memory.
- `shutdown-on-close` only acts when the library was loaded by `library-name` (the autoconfiguration owns it); a library you initialised yourself is never shut down by Spring.

## Native library loading

`VesperaBridge.init("crateName")` tries two paths in order:

1. **Bundled** — looks up `native/{os}-{arch}/{libname}` inside the running JAR's classpath. If present, it is extracted **content-addressed** to `<dir>/vespera-native-<sha256-prefix>/<libname>` and loaded via `System.load`. `<dir>` is the first of: system property `vespera.native.dir`, environment variable `VESPERA_NATIVE_DIR`, or the per-user default `<java.io.tmpdir>/vespera-<user.name>` (sanitized to `[A-Za-z0-9._-]`). A restart reuses the existing file when its SHA-256 matches the bundled library (no new copy per JVM start, nothing left to clean up on exit); a new extraction is written to a temp file in that directory and published with an atomic rename, so concurrent JVMs never load a half-written file. Point `vespera.native.dir` at an exec-permitted, persistent directory when `/tmp` is mounted `noexec` (or is wiped). Older bridge versions left `vespera-<random>.(dll|so|dylib)` copies in `java.io.tmpdir`; those older than one hour are removed best-effort on load (files still in use are skipped) — disable with `-Dvespera.native.cleanupLegacy=false`. Directories of previous library versions (`vespera-native-<other hash>`) are not deleted automatically.

   **Directory safety (POSIX).** The base and hash directories are created with mode `0700` and, before a cached copy is trusted, verified to be real directories (symbolic links are rejected), owned by the current user, and not group- or other-writable; an explicit `vespera.native.dir` / `VESPERA_NATIVE_DIR` is subject to the same checks. The extracted library is made owner read/execute only (`0500`). If verification fails — for example a squatted, shared (`1777`-style) or symlinked directory — the library is extracted into a fresh private `vespera-native-*` temp directory for this JVM only (no reuse, removed on exit where possible) and a warning is logged. **Directory safety (Windows / non-POSIX).** The same guarantee comes from the owner and the ACL: the base and hash directories must be real directories (no symbolic link or junction), owned by the current user, and their ACL must not grant write-like rights (`WRITE_DATA`, `APPEND_DATA`, `DELETE_CHILD`, `DELETE`, `WRITE_ACL`, `WRITE_OWNER`) to anyone but the owner, `SYSTEM` or `Administrators` (inherit-only entries, `DENY` entries and SIDs that do not resolve to an account are ignored). A predictable directory that fails this — e.g. one another user created first under a shared `java.io.tmpdir` to plant a DLL — is not reused: the library is extracted into a fresh, randomly named `vespera-native-*` temp directory for this JVM and a warning is logged. A file system without ACLs gets the same fallback.
2. **Fallback** — `System.loadLibrary("crateName")` searches `java.library.path`.

The bundled lookup is a plain classpath resource lookup, so the library may come from the application jar or from any per-platform native jar on the runtime classpath (`myapp-native-0.1.0-linux-x86_64.jar`, see [Per-platform native jars](#per-platform-native-jars-publish-once-per-platform-consume-one)). When neither path works, the `UnsatisfiedLinkError` names the host platform, the paths tried, the platforms the classpath **does** bundle the library for (e.g. `the classpath bundles libmy_app.so only for [macos-aarch64, windows-x86_64]` — the deployment is missing its platform's jar), or the other `native/<platform>/` libraries found when the `init` name matches none, followed by the `java.library.path` failure.

**musl vs glibc (Linux).** A glibc `.so` cannot be loaded into a musl JVM (Alpine) and vice versa, so musl builds live under their own `native/linux-<arch>-musl/` folder. On Linux the bridge detects the libc the JVM itself is linked against from `/proc/self/maps` (no process is spawned; a mapped `ld-musl-*` means musl) — falling back, without `/proc`, to "`/lib/ld-musl-*` exists and no `ld-linux*` loader does" — and can be forced with `-Dvespera.native.libc=musl|glibc`. On musl it tries `native/linux-<arch>-musl/` first and only when no musl library is bundled loads `native/linux-<arch>/` with a WARN (that load most likely fails); glibc JVMs only ever look at `native/linux-<arch>/`. Build the musl library with a `*-linux-musl` Rust target (a cdylib needs `RUSTFLAGS="-C target-feature=-crt-static"`).

The supported platforms are `linux-x86_64`, `linux-aarch64`, `linux-x86_64-musl`, `linux-aarch64-musl`, `macos-x86_64`, `macos-aarch64`, `windows-x86_64`. Place the cdylib at `src/main/resources/native/{platform}/` to bundle it; see [`examples/rust-jni-demo/java/demo-app/build.gradle.kts`](../../examples/rust-jni-demo/java/demo-app/build.gradle.kts) for a working Gradle task.

## GraalVM native image

The JAR ships GraalVM reachability metadata under `META-INF/native-image/kr.devfive/vespera-bridge/`, which `native-image` picks up from the classpath on its own (no flags, no agent run, nothing to copy):

| File | Registers |
|---|---|
| `jni-config.json` | Everything the Rust side looks up through JNI: `InputStream.read(byte[])` / `close()`, `OutputStream.write(byte[],int,int)` / `flush()`, `Consumer.accept`, `CompletableFuture.complete` (streaming and async callbacks), the `(String)` constructors of the exceptions it throws (`IOException`, `IllegalArgumentException`, `RuntimeException`), and every JDK class the `jni` crate binds internally (`Throwable`, `Thread`, `Class`, `ClassLoader`, its exception types, ...) with all public members — its bindings resolve every member on first use, and a single missing one recurses inside its own error handling until the request thread overflows its stack. |
| `resource-config.json` | Bundled libraries `native/<platform>/<lib>` and `vespera-bridge.properties`. |
| `reflect-config.json` | `Thread.isVirtual()` (looked up reflectively; the bridge targets Java 17). |

With Spring Boot, the autoconfigurations also contribute Spring AOT hints via `@ImportRuntimeHints`: `VesperaBridgeRuntimeHints` (servlet only) for the proxy handler method the CORS preflight mapping resolves by reflection, and `VesperaSecurityRuntimeHints` (servlet and WebFlux) for Spring Security's `Authentication.getAuthorities()` / `GrantedAuthority.getAuthority()` when Spring Security is present (forwarded principal authorities). Anonymous authentication is recognised by class name, so `anonymousUser` is never forwarded even without a reflection hint. So a Spring Boot app that already builds natively only needs the `org.graalvm.buildtools.native` plugin:

```kotlin
plugins {
    id("org.springframework.boot") version "4.1.2"
    id("org.graalvm.buildtools.native") version "1.1.14"   // the version Spring Boot manages
    id("kr.devfive.vespera-bridge") version "..."
}
```

```bash
# JAVA_HOME = GraalVM for JDK 25 (Spring Boot 4) / GraalVM for JDK 17+ (Spring Boot 3.5)
./gradlew nativeCompile
./build/native/nativeCompile/my-app
```

The repository's demo app builds the same way (`./gradlew -PnativeImage :demo-app:nativeCompile` in `examples/rust-jni-demo/java`); `scripts/native-image-smoke.sh` drives every dispatch mode against the executable, and the manual `Native Image` workflow (`.github/workflows/native-image.yml`) runs both on ubuntu.

**Limitations**

- **The Rust library stays a separate file.** A native image cannot statically link a JNI cdylib: `VesperaBridge.init` extracts the bundled `native/<platform>/` library (embedded as a resource) and `System.load`s it exactly as on the JVM, so the extraction directory rules above still apply. Ship it next to the executable and use `java.library.path` instead if you would rather not embed it: drop it from the resources and start with `-Djava.library.path=<dir>`.
- **One platform per image.** Only the host platform's library is useful; bundle just that one (the Gradle plugin's default) to keep the executable small.
- **Same release for both halves**, as on the JVM: the metadata matches the JNI lookups of the `vespera_jni` version from the same release. A cdylib from another release may look up something that is not registered, which fails only inside the image (a JNI lookup error, a streaming callback falling back to a slower path, or a `StackOverflowError` from the `jni` crate's recursive lookup).
- **Charsets.** `VesperaResponse.bodyString()` and form decoding honour the `charset` parameter; a native image only contains the charsets GraalVM includes by default, so add `-H:+AddAllCharsets` if your Rust handlers answer in charsets other than UTF-8 / ISO-8859-1 / US-ASCII.
- **Your own app's reflection** (DTOs Jackson binds in Java code, and so on) is outside this metadata; vespera's DTOs live in Rust and need none.
- Tested with GraalVM CE 25 and Spring Boot 4.1; the metadata uses the classic `*-config.json` format, which GraalVM 22.3 and later read.

## SBOM and provenance

Each release publishes a CycloneDX SBOM next to the JAR on Maven Central
(`kr.devfive:vespera-bridge:<version>:cyclonedx@json`, i.e.
`vespera-bridge-<version>-cyclonedx.json`), and the release workflow attests the
JARs and SBOM with GitHub build provenance. Verify a downloaded JAR with
`gh attestation verify vespera-bridge-<version>.jar --repo dev-five-git/vespera`;
see [SECURITY.md](../../SECURITY.md#verifying-releases) for the SBOM attestation
and the Gradle plugin.

## End-to-end example

See [`examples/rust-jni-demo`](../../examples/rust-jni-demo/) for a complete Rust + Spring Boot integration including build scripts, native bundling, and a curl smoke test. Its `java/demo-reactive-app` module is the WebFlux variant (reactor-netty, no servlet API), whose `ReactiveProxyE2ETest` runs against the real cdylib.

## Breaking changes after 0.5.0

> **Behaviour changes in 0.6 (no code change required, but observable):** the buffered-response cap answers `500` instead of `413` (item 13); CORS preflights are routed to Rust by default (item 14); proxy errors keep filter-set headers and no longer echo exception messages (item 15); the `app` of `DispatchEvent` / the metrics tag is bounded (item 16); a missing native library answers `503` (item 17); a bridge/native release mismatch fails `init` (item 18).

1. **Spring is a provided dependency.** The bridge no longer exports `spring-boot-starter-web` / `jackson-databind`; see [Dependencies](#dependencies--spring-is-provided-by-your-app). Apps that got Spring MVC or Jackson only through the bridge must declare them.
2. **`HEAD` responses advertise the real representation length.** axum answers `HEAD` with an empty body but the `content-length` of the matching `GET`; the proxy now forwards that value (in every dispatch mode) instead of `Content-Length: 0`. When Rust declares no length and sends no body, `Content-Length` is left unset rather than `0`.
3. **Native library extraction location.** The bundled cdylib is extracted to `vespera-native-<sha256-prefix>/` under `vespera.native.dir` / `VESPERA_NATIVE_DIR` / the per-user `<java.io.tmpdir>/vespera-<user.name>` and kept for reuse (only from a verified-private directory on POSIX), instead of a fresh `deleteOnExit` temp file per start; see [Native library loading](#native-library-loading).
4. **Autoconfiguration guard.** `VesperaBridgeAutoConfiguration` is now an `@AutoConfiguration` that also requires Spring MVC (`DispatcherServlet`) and the servlet API on the classpath.
5. **Native defaults now bound resources.** Buffered request bodies are capped at 64 MiB (`413`), buffered responses at 256 MiB (`500`), and `dispatchAsync` at 4096 in-flight calls (`503`). Raise or disable them (`0`) via `vespera.bridge.native.*` or the `vespera.limits.*` / `vespera.async.*` system properties. A per-dispatch deadline is available (`dispatch-timeout`, off by default).
6. **`SYNC`/`DIRECT` run on the shared runtime** (see [Synchronous dispatch runtime semantics](#synchronous-dispatch-runtime-semantics-background-tasks)).
7. **`dispatchStreaming` / `dispatchFullStreaming` throw `IOException`** when the response fails after body bytes were written (previously a synthetic 500 header after a partial body), and `dispatchStreaming` now hoists 422 `validation_errors`.
8. **The new `configure*` methods return `boolean`** (`false` + a logged warning when the native side already fixed the value). `configureStreaming(int, int)` keeps its 0.5.x `void` signature for binary compatibility (a not-applied value is logged).
9. **The bridge JAR and the native library must come from the same release.** The `configure*0` JNI hooks now return `boolean`; a new JAR paired with an older native library would bind them to `void` symbols. Rebuild the cdylib against the matching `vespera` crate version whenever you upgrade `vespera-bridge`.
10. **App selection is restrictable and the app header is no longer forwarded.** New `vespera.bridge.exposed-apps`; the `X-Vespera-App` header (or whatever `app-header` names) is stripped before dispatch — Rust handlers no longer see it (each app is its own router, so a handler already knows which app serves it). See [Security](#security-the-app-header-is-client-controlled).
11. **Non-ASCII header values** now cross the wire under the Latin-1 mapping (see [Binary wire format](#binary-wire-format)) instead of being dropped (responses) or double-encoded (requests).
12. **Consumed form-urlencoded bodies are rebuilt** from the request parameters (see [VesperaProxyController behaviour](#vesperaproxycontroller-behaviour)); a resolver's `BIDIRECTIONAL_STREAMING` choice for a form request becomes `STREAMING`.
13. **Request ids are on by default.** Responses carry `X-Request-Id` (an incoming valid id is reused, otherwise a UUID is generated); disable with `vespera.bridge.request-id.enabled=false`. Controllers built by hand with the old constructors have request ids off.
14. **The forwarded path is the application path.** The servlet context path, a prefix servlet mapping and `;` path parameters (e.g. `;jsessionid`) are stripped before dispatch.
15. **Proxy-generated errors are `text/plain`** (the same shape as Rust's `error_wire`) instead of going through Spring's `/error` page; the unknown-app `404` no longer echoes the requested app name.
16. **`ASYNC` responses get no Spring `Range` handling** (see item 24 for the return type).
17. **Gradle plugin `cargoBuild` is a `CargoBuildTask`**, not an `Exec`; build scripts that configure it as `Exec` must be updated.
18. **Buffered-response cap: `500`, enforced natively.** `vespera.bridge.max-buffered-response-bytes` is forwarded to the native cap (Rust stops before buffering the body) and an oversized `SYNC` / `ASYNC` response is answered `500` instead of `413` (413 means *request* too large). The Spring default (64 MiB) now also applies to the native side unless `vespera.bridge.native.max-buffered-response-bytes` / `vespera.limits.maxBufferedResponseBytes` / `VESPERA_MAX_BUFFERED_RESPONSE_BYTES` set it.
19. **CORS preflights reach Rust by default** (`vespera.bridge.cors=rust`); set `vespera.bridge.cors=spring` to keep Spring's preflight handling. Duplicate `Access-Control-*` headers are merged per owner. See [CORS](#cors).
20. **Proxy-written errors** use the fixed body `vespera dispatch failed` for unexpected exceptions (details only in the WARN log) and keep headers set by servlet filters (`resetBuffer()` + dropping only the proxy's own headers, instead of `reset()`).
21. **Bounded app names.** `DispatchEvent.app()` / the `app` metric tag / the WARN log report `UNKNOWN` for client-selected app names not known to be registered (previously the raw header value; an allowlist-rejected app was reported as `_default`).
22. **Missing native library → `503`.** The proxy answers `503` instead of throwing `UnsatisfiedLinkError`; new `VesperaBridge.isShutdown()`; new `vespera` Actuator health indicator.
23. **Version check.** `VesperaBridge.init` fails when the native library's `major.minor` differs from the bridge's (`-Dvespera.native.versionCheck=warn|off` to relax).
24. **ASYNC** returns a `DeferredResult` (was a `CompletableFuture`) from `proxy(...)`, records a Spring async timeout once as `503`, and serves the body as a zero-copy `InputStreamResource` view of the wire response (no `Range` handling, same as before).

## 0.2.0 breaking changes

### 1. Autoconfigured default `DispatchModeResolver` flipped to `SmartDispatchModeResolver`

Pre-0.2.0 the autoconfigured default was [`BidirectionalStreamingDispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/BidirectionalStreamingDispatchModeResolver.java) — every request that may carry a body streamed both ways, ~24.1 µs per round-trip uniform. Since 0.2.0 the default is [`SmartDispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/SmartDispatchModeResolver.java) — small bounded safe requests take `DIRECT` (~2.2 µs), small unsafe requests take `SYNC` (~3.2 µs), everything else still streams (~24.1 µs).

| Request shape | Pre-0.2.0 mode | 0.2.0+ mode |
|---|---|---|
| Small/bodyless safe (GET/HEAD/OPTIONS, ≤ 1 MiB CL or no CL) | `STREAMING` / `BIDIRECTIONAL_STREAMING` | `DIRECT` |
| Small unsafe (POST/PUT/PATCH/DELETE, ≤ 256 KiB CL) | `BIDIRECTIONAL_STREAMING` | `SYNC` |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | `BIDIRECTIONAL_STREAMING` |

Trade-offs the new default makes:
- **DIRECT** writes the wire response straight into a pooled per-thread direct `ByteBuffer` (64 KiB → `vespera.direct.maxBufferBytes`, default 4 MiB).  Responses larger than the pooled buffer trigger a single retry with a bigger buffer, which **re-runs the Rust handler** — which is why DIRECT is gated on safe methods only.
- **SYNC** fully buffers the response on the JVM heap.  The 256 KiB request-size gate keeps the response size reasonable for JSON-RPC-shaped traffic; large or unknown-length bodies still stream.

**Opt out** (restore the pre-0.2.0 default):

```yaml
vespera:
  bridge:
    dispatch-mode: bidirectional-streaming
```

Or register a custom [`DispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/DispatchModeResolver.java) bean — `@ConditionalOnMissingBean` ensures it wins over both the property and the autoconfigured default.

### 2. `DecodedResponse.body()` returns `ByteBuffer`

`DecodedResponse.body()` now returns a read-only `java.nio.ByteBuffer` (zero-copy view over the wire bytes); the owned `byte[]` materialisation moved to `DecodedResponse.bodyBytes()`.  Callers that previously consumed `body()` as `byte[]` must switch to `bodyBytes()` (or read directly from the buffer).

## Migrating from the JSON-envelope bridge (≤ 0.0.13)

The pre-0.0.14 bridge used `dispatch(String) → String` with base64-encoded binary bodies. Migration:

| Before | After |
|---|---|
| `VesperaBridge.dispatch(json)` | `encodeRequest(...)` → `dispatchBytes(...)` → `decodeResponse(...)` |
| `body_bytes_b64` field on the response JSON | raw body bytes after the wire header (no base64) |
| ~33 % size overhead on binary bodies | zero overhead |

Existing users of `VesperaProxyController` need no code change — the controller was rewritten to the new wire path internally. Direct callers of `VesperaBridge.dispatch(String)` must update; the old method was removed in 0.0.14.
