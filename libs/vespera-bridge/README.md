# vespera-bridge

JNI bridge that lets a Java/Spring application embed a Rust [`vespera`](../../) axum router in-process — no TCP, no JSON envelope overhead, raw bytes from end to end.

```xml
<dependency>
  <groupId>kr.devfive</groupId>
  <artifactId>vespera-bridge</artifactId>
  <version>0.1.1</version>
</dependency>
```

```kotlin
dependencies {
    implementation("kr.devfive:vespera-bridge:0.1.1")
}
```

### One-line setup via the Gradle plugin (recommended)

For Spring Boot apps the [`kr.devfive.vespera-bridge`](../vespera-bridge-gradle-plugin/) Gradle plugin replaces the ~22 lines of native-library-bundling boilerplate with a 5-line `vespera { ... }` block:

```kotlin
plugins {
    id("kr.devfive.vespera-bridge") version "0.0.15"
}

vespera {
    crateName.set("my_rust_lib")
    cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
    bridgeVersion.set("0.1.1")
}
```

The plugin auto-wires `bundleNativeLib` (cdylib → `resources/native/<os>-<arch>/`), `processResources` dependency, and the `vespera-bridge` `implementation` dependency.  See [`examples/rust-jni-demo/java/demo-app/build.gradle.kts`](../../examples/rust-jni-demo/java/demo-app/build.gradle.kts) for a real working example (the demo dogfoods the plugin end-to-end with a 1 MiB SHA256-verified bidirectional streaming round-trip).

## Two-line integration

```java
@SpringBootApplication
@ComponentScan(basePackages = {"com.example.app", "com.devfive.vespera.bridge"})
public class MyApp {
    public static void main(String[] args) {
        VesperaBridge.init("my_rust_lib");      // ← loads cdylib (bundled or system)
        SpringApplication.run(MyApp.class, args);
    }
}
```

`VesperaProxyController` is **autoconfigured** (via Spring Boot `AutoConfiguration.imports`) and forwards every HTTP request to Rust.  You write zero controller code on the Java side, **zero `application.yml` config**, and **zero `import` lines** beyond the Spring Boot starter — the routes published in vespera's generated `openapi.json` are reachable at the same URLs through Spring.

## Zero-config defaults

Out of the box the autoconfigure module wires up:

| Concern | Default | Override |
|---|---|---|
| **App selection** | Read `X-Vespera-App` request header; absent → default app | Property `vespera.bridge.app-header`, or custom [`AppNameResolver`](src/main/java/com/devfive/vespera/bridge/AppNameResolver.java) bean |
| **Dispatch mode** | [`BIDIRECTIONAL_STREAMING`](src/main/java/com/devfive/vespera/bridge/DispatchMode.java) for every request — safe for any payload size, transparent for the Rust router | Custom [`DispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/DispatchModeResolver.java) bean |
| **URL pattern** | Single `@RequestMapping("/**")` catch-all — every vespera router URL exactly mirrors the published OpenAPI path | Set `vespera.bridge.controller-enabled: false` and supply your own controller |
| **Body handling** | Servlet `InputStream` straight through to Rust (no buffering) for streaming modes; full read for sync/async | (encoded by the chosen `DispatchMode`) |

Why `BIDIRECTIONAL_STREAMING` as the default mode? It's the only mode that processes every payload size correctly without dispatch-time hints:

- **Tiny request / tiny response** (`/health` → `"ok"`): processed as a single chunk, negligible overhead.
- **Small JSON RPC** (`/users` → `{...}`): single chunk both ways.
- **Multi-GB upload + multi-GB download**: chunk-bounded both ways, ~32 KiB resident.

This means the Spring endpoints **always** mirror vespera's `openapi.json` — there is no URL prefix or mode-detection heuristic that could diverge from the Rust router's view of the world.

## Customization

All defaults are individually replaceable.  Start with properties for minor tweaks, drop in a `@Bean` for serious behavior changes, or disable the autoconfigured controller entirely.

### 1. Tweak via `application.yml`

```yaml
vespera:
  bridge:
    app-header: X-My-App        # change the header that selects the app
    controller-enabled: true     # set false to disable our controller
```

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
        return ResponseEntity.status(d.status()).body(d.body());
    }
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
  so only retry idempotent requests.
- `Integer.MIN_VALUE`: response exceeds 2 GiB (unrepresentable).

`dispatchDirectPooled(byte[] wireRequest, boolean retryOnOverflow)`
wraps the raw call with per-thread reusable direct buffers (64 KiB
initial, doubling up to the `vespera.direct.maxBufferBytes` system
property, default 4 MiB) and returns a read-only view of the response
valid until the next dispatch on the same thread.  On response
overflow it throws `BufferTooSmallException(requiredSize)` unless
`retryOnOverflow` is `true` — pass `true` only for idempotent
requests, because the retry dispatches again.

The fastest variant skips the intermediate wire `byte[]` entirely —
`dispatchDirectPooled(appName, method, path, query, headers, body,
retryOnOverflow)` encodes straight into the pooled direct buffer via
`encodeRequestInto(...)`, so the body is copied heap→direct exactly
once.  `encodeRequestInto(..., ByteBuffer target)` is also public for
callers managing their own buffers; it returns the bytes written or
`-(required)` without touching the buffer when `target` is too small
(an encoding-side signal — no dispatch has run, growing and retrying
is always safe, unlike the response-overflow retry).

For the Spring proxy, `DispatchMode.DIRECT` is **opt-in**: the default
resolver stays `BIDIRECTIONAL_STREAMING` for every request.  Register
a `SmartDispatchModeResolver` bean to route small bounded idempotent
requests through DIRECT:

```java
@Bean
public DispatchModeResolver dispatchModeResolver() {
    // DIRECT only when Content-Length is known, <= 256 KiB, and the
    // method is idempotent (GET/HEAD/PUT/DELETE/OPTIONS); everything
    // else falls back to BIDIRECTIONAL_STREAMING.
    return new SmartDispatchModeResolver();
}
```

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
System.out.println(new String(resp.body())); // the raw response body
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

Memory characteristics: **roughly 16 KiB chunk buffer + a 16-slot
mpsc channel buffer** in Rust, plus normal JVM `byte[]` chunks. A
1 GiB upload paired with a 1 GiB download runs in ~500 KiB resident
memory on each side. Backpressure is enforced naturally — if axum
reads slowly, `InputStream.read()` blocks on the bounded channel.

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
assert Arrays.equals(pdf, resp.body());      // exact round-trip
```

A Rust handler returning a binary response (e.g. `image/png`) flows the same way: `VesperaProxyController` inspects the response `Content-Type` and returns `ResponseEntity<byte[]>` for binary content, `ResponseEntity<String>` for text-like content.

## VesperaProxyController behaviour

`@RequestMapping("/**")` catches every HTTP request, regardless of method or content type, and:

1. Collects all incoming headers (lowercased keys).
2. Asks the configured `DispatchModeResolver` which mode serves this request (default: `BIDIRECTIONAL_STREAMING` for everything — servlet input/output streams pass straight through, no body materialisation).
3. For `SYNC` / `ASYNC` / `STREAMING` / `DIRECT` modes the body is read into `byte[]` first, then encoded via `VesperaBridge.encodeRequest(...)` and dispatched through the matching native method.
4. Sync/async responses are decoded via `VesperaBridge.decodeResponse(byte[])` and returned as `ResponseEntity<String>` for text-like `Content-Type` (e.g. `text/*`, `application/json`, `+json`, `+xml`, `application/xml`, `application/javascript`, `application/yaml`, `application/x-www-form-urlencoded`, `application/graphql`), `ResponseEntity<byte[]>` otherwise.  Streaming and DIRECT modes write status/headers and body straight to the servlet response.

Missing `Content-Type` defaults to "text" — matching the long-standing Vespera convention of treating unspecified content as JSON-shaped.

## Native library loading

`VesperaBridge.init("crateName")` tries two paths in order:

1. **Bundled** — looks up `native/{os}-{arch}/{libname}` inside the running JAR's classpath. If present, the file is extracted to a temp file (auto-deleted on JVM exit) and loaded via `System.load`.
2. **Fallback** — `System.loadLibrary("crateName")` searches `java.library.path`.

The supported triples are `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`, `windows-x86_64`. Place the cdylib at `src/main/resources/native/{os}-{arch}/` to bundle it; see [`examples/rust-jni-demo/java/demo-app/build.gradle.kts`](../../examples/rust-jni-demo/java/demo-app/build.gradle.kts) for a working Gradle task.

## End-to-end example

See [`examples/rust-jni-demo`](../../examples/rust-jni-demo/) for a complete Rust + Spring Boot integration including build scripts, native bundling, and a curl smoke test.

## Migrating from the JSON-envelope bridge (≤ 0.0.13)

The pre-0.0.14 bridge used `dispatch(String) → String` with base64-encoded binary bodies. Migration:

| Before | After |
|---|---|
| `VesperaBridge.dispatch(json)` | `encodeRequest(...)` → `dispatchBytes(...)` → `decodeResponse(...)` |
| `body_bytes_b64` field on the response JSON | raw body bytes after the wire header (no base64) |
| ~33 % size overhead on binary bodies | zero overhead |

Existing users of `VesperaProxyController` need no code change — the controller was rewritten to the new wire path internally. Direct callers of `VesperaBridge.dispatch(String)` must update; the old method was removed in 0.0.14.
