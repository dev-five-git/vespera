# vespera-bridge

JNI bridge that lets a Java/Spring application embed a Rust [`vespera`](../../) axum router in-process — no TCP, no JSON envelope overhead, raw bytes from end to end.

```xml
<dependency>
  <groupId>kr.devfive</groupId>
  <artifactId>vespera-bridge</artifactId>
  <version>0.5.0</version>
</dependency>
```

```kotlin
dependencies {
    implementation("kr.devfive:vespera-bridge:0.5.0")
}
```

### Version compatibility

- **Bridge ↔ Rust crate:** the `vespera-bridge` version must match the **minor** version of the `vespera` crate your cdylib is built with (bridge `0.5.x` ↔ `vespera = "0.5"`). The JNI symbol set and the binary wire format are only guaranteed stable within a minor line.
- **Spring Boot:** compiled against Spring Boot 4.1 / Spring Framework 7.0; supported on **Spring Boot 3.5.x and 4.x** (Spring Framework 6.2 / 7.0, Jakarta Servlet 6.0+). The Boot 3.5 runtime is verified by the `testBoot3` Gradle task (run automatically after `./gradlew test`), which runs the unit suite against a Spring Boot 3.5 classpath. Spring Boot ≤ 3.4 / `javax.servlet` is not supported.
- **Java:** 17+.

### Dependencies — Spring is provided by your app

`vespera-bridge` declares **no transitive dependencies** (its POM is empty): Spring Boot, Spring MVC, the servlet API and SLF4J are compile-only and must come from the application. Every Spring Boot web app already has them through `spring-boot-starter-web` (Boot 3) / `spring-boot-starter-webmvc` (Boot 4), so nothing changes there. The autoconfiguration only activates in a servlet web application with Spring MVC on the classpath (`@ConditionalOnClass` + `@ConditionalOnWebApplication(type = SERVLET)`), and the non-Spring API (`VesperaBridge` dispatch / `encodeRequest` / `decodeResponse`) runs with no Spring jars at all.

> Up to and including 0.5.0 the bridge exported `spring-boot-starter-web` and `jackson-databind` as `api` dependencies. If your build relied on getting either of them transitively from the bridge, declare it yourself.

### One-line setup via the Gradle plugin (recommended)

For Spring Boot apps the [`kr.devfive.vespera-bridge`](../vespera-bridge-gradle-plugin/) Gradle plugin replaces the ~22 lines of native-library-bundling boilerplate with a 5-line `vespera { ... }` block:

```kotlin
plugins {
    id("kr.devfive.vespera-bridge") version "0.4.1"
}

vespera {
    crateName.set("my_rust_lib")
    cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
    bridgeVersion.set("0.5.0")
}
```

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

Each `targets` key is a bridge platform (`<os>-<arch>`, os `windows` / `macos` / `linux`); its library is read from `<targetDir>/<triple>/<profile>/` and bundled under `native/<os>-<arch>/` with that platform's file name (`.dll` / `lib*.dylib` / `lib*.so`). A key naming the host platform replaces the host build. The cross toolchain/linker must already be installed. Everything under `extraNativeLibs/native/` is merged in; a locally built library wins over a prebuilt one with the same path. `cargoBuild` is never `UP-TO-DATE` (Cargo's own incremental check decides) and does not fingerprint `target/`, `node_modules/` or `.git/`.

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
| **URL pattern** | Single `@RequestMapping("/**")` catch-all — every vespera router URL exactly mirrors the published OpenAPI path | Set `vespera.bridge.controller-enabled: false` and supply your own controller |
| **Body handling** | Servlet `InputStream` straight through to Rust (no buffering) for streaming modes; full read for sync/async | (encoded by the chosen `DispatchMode`) |

Why `smart` as the default mode (since 0.2.0)? Measured on a small `GET /health` round-trip through the real JNI boundary the cheapest safe path per request is 7–11× cheaper than unconditional streaming:

| Request shape | Mode | ns/round-trip |
|---|---|---|
| Small/bodyless + safe (GET/HEAD/OPTIONS, Content-Length absent or ≤ 1 MiB) | `DIRECT` | ~2,200 |
| Small (≤ 256 KiB Content-Length) + unsafe (POST/PUT/PATCH/DELETE) | `SYNC` | ~3,200 |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | ~24,100 |

Trade-offs the new default makes on your behalf:

- **DIRECT** writes the wire response straight into a pooled direct `ByteBuffer` (per-thread, 64 KiB → `vespera.direct.maxBufferBytes` default 4 MiB). On responses larger than the pooled buffer the Java side **retries once with a bigger buffer** by default, which re-runs the Rust handler. This is why DIRECT is gated on safe methods only. Set `vespera.bridge.direct-retry-on-overflow=false` to surface the overflow instead of automatically retrying.
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
    direct-retry-on-overflow: true # set false to avoid DIRECT retry double-execution
    max-buffered-request-bytes: 67108864 # default 64 MiB; 0 = unlimited
    native:                      # Rust-side knobs, applied before the first dispatch
      max-request-bytes: 67108864           # buffered request cap → 413 (0 = unlimited)
      max-streaming-request-bytes: 0        # streamed upload cap → 413 (0 = unlimited, default)
      max-buffered-response-bytes: 268435456 # buffered response cap → 500 (default 256 MiB)
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

`vespera.bridge.direct-retry-on-overflow` defaults to `true` for backward
compatibility with the 0.2.x DIRECT fast path.  When `false`, a DIRECT response
overflow raises the existing `BufferTooSmallException` path in the proxy (HTTP
500 with the required size) instead of growing the response buffer and re-running
the Rust handler.

`vespera.bridge.max-buffered-request-bytes` defaults to 64 MiB (`0` =
unlimited), matching the Rust-side `VESPERA_MAX_REQUEST_BYTES` default.  The
proxy rejects SYNC/ASYNC/DIRECT/STREAMING requests whose buffered body exceeds
the cap with HTTP 413.  Bidirectional
streaming is exempt and remains the path for large or unknown-size uploads.

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

## Binary wire format

Both request and response use the same length-prefixed layout:

```
bytes 0..4    : u32 BE = header_json byte length N
bytes 4..4+N  : UTF-8 JSON
                  (request)  { "v":1, "method", "path",
                               "query"?, "headers"? }
                  (response) { "v":1, "status", "headers",
                               "metadata" }
bytes 4+N..   : raw body bytes (UTF-8 text or binary —
                no encoding applied)
```

- `"v":1` is the protocol version. Mismatched versions return a `400` wire response with an explanatory plain-text body.
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
  `requiredSize` is exact, but **retrying re-runs the Rust handler**,
so only retry safe requests.
- `Integer.MIN_VALUE`: response exceeds 2 GiB (unrepresentable).

`dispatchDirectPooled(byte[] wireRequest, boolean retryOnOverflow)`
wraps the raw call with per-thread reusable direct buffers (64 KiB
initial, doubling up to the `vespera.direct.maxBufferBytes` system
property, default 4 MiB) and returns a read-only view of the response
valid until the next dispatch on the same thread.  On response
overflow it throws `BufferTooSmallException(requiredSize)` unless
`retryOnOverflow` is `true` — pass `true` only for safe
requests, because the retry dispatches again.  In the Spring proxy,
`retryOnOverflow` is additionally gated by
`vespera.bridge.direct-retry-on-overflow` (default `true`; set `false`
to keep DIRECT from automatically double-executing any handler).

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
| Small/bodyless + safe (GET/HEAD/OPTIONS) | `DIRECT` | ~2,200 |
| Small (≤ 256 KiB Content-Length) + unsafe (POST/PUT/PATCH/DELETE) | `SYNC` | ~3,200 |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | ~24,100 |

The safe-method gate on DIRECT matters because a response that
overflows the pooled buffer (`vespera.direct.maxBufferBytes`, default
4 MiB) is retried by default — which re-runs the Rust handler once.
Safe methods are not intended to mutate server state, but the replayed
response may still differ (for example timestamps or generated IDs).
Set `vespera.bridge.direct-retry-on-overflow=false` to surface the
overflow instead.  SYNC never re-runs the handler (safe for POST), but
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

The `DIRECT` fast path keeps per-platform-thread pooled direct
`ByteBuffer`s (64 KiB → `vespera.direct.maxBufferBytes`, default 4 MiB),
and `SYNC`/`DIRECT` keep one current-thread runtime per Java request
thread. On a large servlet pool (e.g. 200 Tomcat threads) the steady-state
cost is therefore bounded but not negligible:

- **Off-heap**: up to `poolThreads × maxBufferBytes` retained direct
  memory once each thread has served a large response (adaptive shrink
  reclaims it after idle dispatches). Lower `vespera.direct.maxBufferBytes`
  if off-heap pressure matters more than the DIRECT copy savings.
- **Threads**: worst case `poolThreads × 4` Tokio blocking threads if many
  handlers use `spawn_blocking` concurrently (the multipart temp-file
  extractor does). Cap Rust's shared async runtime with
  `vespera.runtime.workerThreads` when the JVM's own pools compete for the
  same cores, or when a container CPU limit is below the host CPU count.

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

> Cancellation note: `future.cancel(true)` marks the Java side as
> cancelled but does not abort the in-flight Rust dispatch in this
> release. The Rust task continues to completion and its result is
> discarded.

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

Errors the proxy produces itself (unknown app 404, buffered body too large 413, async backpressure 503, DIRECT overflow, native failures 500) are written directly as `text/plain; charset=utf-8` with the message as body, the same shape as the Rust `error_wire` responses, instead of going through Spring's `/error` page (whose shape depends on `Accept`).

### Panics, 5xx and metrics

A Rust panic answers `500` with `"panicked":"true"` in the response wire `metadata` (`DecodedResponse.metadata().get("panicked")`). Every dispatch that ends in a panic or a `5xx` is logged at WARN with method, path, app, mode, status, latency and request id. For metrics or tracing, register `DispatchObserver` beans; each is called once per request with a `DispatchEvent(app, mode, method, path, status, durationNanos, panicked, requestId)` (observer exceptions are logged and swallowed). When Micrometer is on the classpath and a `MeterRegistry` bean exists (e.g. Spring Boot Actuator), a `MicrometerDispatchObserver` is autoconfigured: timer `vespera.bridge.dispatch` tagged `app`, `mode`, `status` and `outcome` (`SUCCESS`, `CLIENT_ERROR`, `SERVER_ERROR`, `PANIC`, ...). Micrometer stays an optional dependency.

### Redeploy, class loaders and shutdown

```yaml
vespera.bridge:
  library-name: my_rust_lib   # VesperaBridge.init(...) during context startup (if not already loaded)
  shutdown-on-close: false    # default; true = stop the Rust runtime when the context closes
  shutdown-timeout: 10s
```

- A JVM can load a native library through **one class loader only**, and it is never unloaded. Loading it again from a redeployed web app fails with `UnsatisfiedLinkError: ... already loaded in another classloader`; the bridge rethrows it with guidance. On servlet containers that redeploy or host several apps, put `vespera-bridge` and the library in the container's shared lib (e.g. Tomcat `$CATALINA_BASE/lib`) or run one deployment per JVM, and restart the JVM to pick up a new native build.
- `VesperaBridge.shutdown(Duration)` stops the shared Tokio runtime (waiting up to the timeout for running tasks) and returns `false` if it was already shut down (or the library is not loaded). It is one-way: afterwards every dispatch answers `503`, and the runtime cannot be restarted in the same process. Registered apps and the loaded library stay in memory.
- `shutdown-on-close` only acts when the library was loaded by `library-name` (the autoconfiguration owns it); a library you initialised yourself is never shut down by Spring.

## Native library loading

`VesperaBridge.init("crateName")` tries two paths in order:

1. **Bundled** — looks up `native/{os}-{arch}/{libname}` inside the running JAR's classpath. If present, it is extracted **content-addressed** to `<dir>/vespera-native-<sha256-prefix>/<libname>` and loaded via `System.load`. `<dir>` is the first of: system property `vespera.native.dir`, environment variable `VESPERA_NATIVE_DIR`, or the per-user default `<java.io.tmpdir>/vespera-<user.name>` (sanitized to `[A-Za-z0-9._-]`). A restart reuses the existing file when its SHA-256 matches the bundled library (no new copy per JVM start, nothing left to clean up on exit); a new extraction is written to a temp file in that directory and published with an atomic rename, so concurrent JVMs never load a half-written file. Point `vespera.native.dir` at an exec-permitted, persistent directory when `/tmp` is mounted `noexec` (or is wiped). Older bridge versions left `vespera-<random>.(dll|so|dylib)` copies in `java.io.tmpdir`; those older than one hour are removed best-effort on load (files still in use are skipped) — disable with `-Dvespera.native.cleanupLegacy=false`. Directories of previous library versions (`vespera-native-<other hash>`) are not deleted automatically.

   **Directory safety (POSIX).** The base and hash directories are created with mode `0700` and, before a cached copy is trusted, verified to be real directories (symbolic links are rejected), owned by the current user, and not group- or other-writable; an explicit `vespera.native.dir` / `VESPERA_NATIVE_DIR` is subject to the same checks. The extracted library is made owner read/execute only (`0500`). If verification fails — for example a squatted, shared (`1777`-style) or symlinked directory — the library is extracted into a fresh private `vespera-native-*` temp directory for this JVM only (no reuse, removed on exit where possible) and a warning is logged. On Windows (no POSIX attributes) the directories are used as-is; the default already sits under the per-user `%TEMP%`.
2. **Fallback** — `System.loadLibrary("crateName")` searches `java.library.path`.

The supported triples are `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`, `windows-x86_64`. Place the cdylib at `src/main/resources/native/{os}-{arch}/` to bundle it; see [`examples/rust-jni-demo/java/demo-app/build.gradle.kts`](../../examples/rust-jni-demo/java/demo-app/build.gradle.kts) for a working Gradle task.

## End-to-end example

See [`examples/rust-jni-demo`](../../examples/rust-jni-demo/) for a complete Rust + Spring Boot integration including build scripts, native bundling, and a curl smoke test.

## Breaking changes after 0.5.0

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
