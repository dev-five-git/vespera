# vespera-bridge

JNI bridge that lets a Java/Spring application embed a Rust [`vespera`](../../) axum router in-process — no TCP, no JSON envelope overhead, raw bytes from end to end.

```xml
<dependency>
  <groupId>kr.devfive</groupId>
  <artifactId>vespera-bridge</artifactId>
  <version>0.2.0</version>
</dependency>
```

```kotlin
dependencies {
    implementation("kr.devfive:vespera-bridge:0.2.0")
}
```

### One-line setup via the Gradle plugin (recommended)

For Spring Boot apps the [`kr.devfive.vespera-bridge`](../vespera-bridge-gradle-plugin/) Gradle plugin replaces the ~22 lines of native-library-bundling boilerplate with a 5-line `vespera { ... }` block:

```kotlin
plugins {
    id("kr.devfive.vespera-bridge") version "0.1.1"
}

vespera {
    crateName.set("my_rust_lib")
    cargoRoot.set(rootProject.layout.projectDirectory.dir("../.."))
    bridgeVersion.set("0.2.0")
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
| **Dispatch mode** | [`SmartDispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/SmartDispatchModeResolver.java) since 0.2.0 — picks per request: [`DIRECT`](src/main/java/com/devfive/vespera/bridge/DispatchMode.java) (pooled direct buffers, no JNI array copies) for small/bodyless idempotent requests (GET/HEAD/PUT/DELETE/OPTIONS, Content-Length absent or ≤ 1 MiB) ~2.2 µs; `SYNC` (heap-buffered) for small non-idempotent (POST/PATCH ≤ 256 KiB) ~3.2 µs; `BIDIRECTIONAL_STREAMING` for the rest ~24.1 µs | Property `vespera.bridge.dispatch-mode: bidirectional-streaming` (opt out, restore pre-0.2.0 default), or custom [`DispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/DispatchModeResolver.java) bean |
| **URL pattern** | Single `@RequestMapping("/**")` catch-all — every vespera router URL exactly mirrors the published OpenAPI path | Set `vespera.bridge.controller-enabled: false` and supply your own controller |
| **Body handling** | Servlet `InputStream` straight through to Rust (no buffering) for streaming modes; full read for sync/async | (encoded by the chosen `DispatchMode`) |

Why `smart` as the default mode (since 0.2.0)? Measured on a small `GET /health` round-trip through the real JNI boundary the cheapest safe path per request is 7–11× cheaper than unconditional streaming:

| Request shape | Mode | ns/round-trip |
|---|---|---|
| Small/bodyless + idempotent (GET/HEAD/PUT/DELETE/OPTIONS, Content-Length absent or ≤ 1 MiB) | `DIRECT` | ~2,200 |
| Small (≤ 256 KiB Content-Length) + non-idempotent (POST/PATCH) | `SYNC` | ~3,200 |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | ~24,100 |

Trade-offs the new default makes on your behalf:

- **DIRECT** writes the wire response straight into a pooled direct `ByteBuffer` (per-thread, 64 KiB → `vespera.direct.maxBufferBytes` default 4 MiB). On responses larger than the pooled buffer the Java side **retries once with a bigger buffer**, which re-runs the Rust handler. This is why DIRECT is gated on idempotent methods only.
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
        return ResponseEntity.status(d.status()).body(d.bodyBytes());
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
| Small/bodyless + idempotent (GET/HEAD/PUT/DELETE/OPTIONS) | `DIRECT` | ~2,200 |
| Small (≤ 256 KiB Content-Length) + non-idempotent (POST/PATCH) | `SYNC` | ~3,200 |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | ~24,100 |

The idempotency gate on DIRECT matters because a response that
overflows the pooled buffer (`vespera.direct.maxBufferBytes`, default
4 MiB) is retried — which re-runs the Rust handler once.  SYNC never
re-runs the handler (safe for POST), but buffers the full response on
the heap, which the request-size gate keeps reasonable for
JSON-RPC-shaped traffic.

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

1. **Programmatic setter** — `VesperaBridge.configureStreaming(chunkBytes, channelCapacity)` (Java API, call before or after init)
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

1. Collects all incoming headers (lowercased keys).
2. Asks the configured `DispatchModeResolver` which mode serves this request (default since 0.2.0: `SmartDispatchModeResolver` — DIRECT for small/bodyless idempotent requests, SYNC for small non-idempotent requests, BIDIRECTIONAL_STREAMING for everything else; opt out with `vespera.bridge.dispatch-mode=bidirectional-streaming`).
3. For `SYNC` / `ASYNC` / `STREAMING` / `DIRECT` modes the body is read into `byte[]` first (bodyless requests — explicit `Content-Length: 0`, e.g. the small idempotent GETs the SmartDispatch resolver routes through DIRECT — skip the read and reuse a shared empty array), then encoded via `VesperaBridge.encodeRequest(...)` and dispatched through the matching native method.
4. Sync/async responses are parsed straight from the wire response via the allocation-lean `WireHeaderReader` (status + headers) and returned as `ResponseEntity<byte[]>` for **every** `Content-Type` — the body is sliced once from the wire tail; the `Content-Type` header is carried verbatim, so no text/binary branching is needed.  Streaming and DIRECT modes write status/headers and body straight to the servlet response.

## Native library loading

`VesperaBridge.init("crateName")` tries two paths in order:

1. **Bundled** — looks up `native/{os}-{arch}/{libname}` inside the running JAR's classpath. If present, the file is extracted to a temp file (auto-deleted on JVM exit) and loaded via `System.load`.
2. **Fallback** — `System.loadLibrary("crateName")` searches `java.library.path`.

The supported triples are `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`, `windows-x86_64`. Place the cdylib at `src/main/resources/native/{os}-{arch}/` to bundle it; see [`examples/rust-jni-demo/java/demo-app/build.gradle.kts`](../../examples/rust-jni-demo/java/demo-app/build.gradle.kts) for a working Gradle task.

## End-to-end example

See [`examples/rust-jni-demo`](../../examples/rust-jni-demo/) for a complete Rust + Spring Boot integration including build scripts, native bundling, and a curl smoke test.

## 0.2.0 breaking changes

### 1. Autoconfigured default `DispatchModeResolver` flipped to `SmartDispatchModeResolver`

Pre-0.2.0 the autoconfigured default was [`BidirectionalStreamingDispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/BidirectionalStreamingDispatchModeResolver.java) — every request that may carry a body streamed both ways, ~24.1 µs per round-trip uniform. Since 0.2.0 the default is [`SmartDispatchModeResolver`](src/main/java/com/devfive/vespera/bridge/SmartDispatchModeResolver.java) — small bounded idempotent requests take `DIRECT` (~2.2 µs), small non-idempotent take `SYNC` (~3.2 µs), everything else still streams (~24.1 µs).

| Request shape | Pre-0.2.0 mode | 0.2.0+ mode |
|---|---|---|
| Small/bodyless idempotent (GET/HEAD/PUT/DELETE/OPTIONS, ≤ 1 MiB CL or no CL) | `STREAMING` / `BIDIRECTIONAL_STREAMING` | `DIRECT` |
| Small non-idempotent (POST/PATCH, ≤ 256 KiB CL) | `BIDIRECTIONAL_STREAMING` | `SYNC` |
| Large or unknown-length body | `BIDIRECTIONAL_STREAMING` | `BIDIRECTIONAL_STREAMING` |

Trade-offs the new default makes:
- **DIRECT** writes the wire response straight into a pooled per-thread direct `ByteBuffer` (64 KiB → `vespera.direct.maxBufferBytes`, default 4 MiB).  Responses larger than the pooled buffer trigger a single retry with a bigger buffer, which **re-runs the Rust handler** — which is why DIRECT is gated on idempotent methods only.
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
