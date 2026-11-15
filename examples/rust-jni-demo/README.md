# rust-jni-demo

Vespera app that runs in two modes from the same codebase:

| Mode | Transport | How to start |
|------|-----------|--------------|
| **Standalone** | TCP :3000 | `cargo run -p rust-jni-demo` |
| **JNI** | In-process (no network) | Java loads the cdylib |

Both modes use the same `create_app()` → same routes, same logic.

## Prerequisites

- A recent stable Rust (the published crates need 1.88+; the examples may
  use newer language features)
- Java 17+ (for JNI mode)

## Mode A: Standalone Rust Server

```bash
cargo run -p rust-jni-demo
```

```
Server running on http://localhost:3000
  GET  /health
  POST /documents/validate
```

### Test with curl

```bash
# Health check
curl http://localhost:3000/health

# Validate a document
curl -X POST http://localhost:3000/documents/validate \
  -H 'Content-Type: application/json' \
  -d '{
    "documentType": "regulation",
    "title": "Data Protection Policy",
    "content": "This regulation establishes the framework for handling personal data within the organisation.",
    "author": "Kim Minjun",
    "department": "Information Security",
    "classification": "internal",
    "effectiveDate": "2025-01-01"
  }'
```

## Mode B: Java + JNI

Java calls Rust in-process — no HTTP between them.

```
Client ── HTTP ──> Spring Boot ── JNI ──> Rust (axum router)
```

### Step 1: Build the Rust shared library

```bash
cargo build -p rust-jni-demo --release
```

Output:
- Linux: `target/release/librust_jni_demo.so`
- macOS: `target/release/librust_jni_demo.dylib`
- Windows: `target/release/rust_jni_demo.dll`

### Step 2: Publish the vespera-bridge JAR and Gradle plugin to mavenLocal

The demo dogfoods the bridge and the `kr.devfive.vespera-bridge` Gradle plugin
built from this repository: `java/settings.gradle.kts` resolves both from
`mavenLocal()` first, at the versions read from
`libs/vespera-bridge/build.gradle.kts` and
`libs/vespera-bridge-gradle-plugin/build.gradle.kts` (the same steps CI runs):

```bash
(cd libs/vespera-bridge-gradle-plugin && ./gradlew publishToMavenLocal)
(cd libs/vespera-bridge && ./gradlew publishToMavenLocal)
```

### Step 3: Build and run the Spring Boot app

The native library is **bundled inside the JAR** — single-file deployment.

```bash
cd examples/rust-jni-demo/java
./gradlew :demo-app:bootJar

# Single file, no -Djava.library.path needed:
java -jar demo-app/build/libs/demo-app-0.1.0.jar

# Windows PowerShell
java "-Djava.library.path=..\..\..\target\release" -jar demo-app\build\libs\demo-app-0.1.0.jar
```

Spring starts on `http://localhost:8080` and proxies every request to Rust:

```bash
curl -X POST http://localhost:8080/documents/validate \
  -H 'Content-Type: application/json' \
  -d '{"documentType":"regulation","title":"Test","content":"Some content here for validation purposes.","author":"Kim","department":"Legal","classification":"internal","effectiveDate":"2025-01-01"}'
```

## Run tests

```bash
cargo test -p rust-jni-demo
```

## Project structure

```
crates/
├── vespera/                # OpenAPI framework + re-exports inprocess/jni via features
├── vespera_inprocess/      # In-process dispatch + app factory (shared FFI pattern)
└── vespera_jni/            # JNI glue: Runtime + JNI symbol (never depended on directly)

libs/
└── vespera-bridge/         # Java JAR (com.devfive.vespera.bridge)

examples/rust-jni-demo/
├── Cargo.toml              # depends on vespera only: features = ["jni", "ffi"]
├── src/
│   ├── lib.rs              # create_app() + admin_app(), registered by
│   │                       # vespera::jni_apps! { .. } and vespera::ffi_apps! { .. }
│   ├── main.rs             # Mode A: vespera::axum::serve on :3000
│   ├── routes/             # default app: documents, health, echo, form,
│   │                       # headers, stream, upload, diag
│   └── admin_routes/       # "admin" app (X-Vespera-App: admin)
├── ffi/                    # C ABI demo (ctypes_demo.py, header_check.c)
└── java/
    ├── demo-app/           # Mode B: Spring Boot (servlet) proxy
    │   └── src/.../DemoApplication.java
    └── demo-reactive-app/  # Spring WebFlux proxy
```

## How it works

### Rust side

```rust
// Cargo.toml — single dependency
// vespera = { features = ["jni", "ffi"] }

// lib.rs — the entire JNI (and C ABI) integration:
pub fn create_app() -> axum::Router {
    vespera!(title = "Document Validation API", version = "0.1.0")
}

pub fn admin_app() -> axum::Router {
    vespera!(dir = "admin_routes", title = "Admin Dashboard API", version = "0.1.0")
}

// One JNI_OnLoad; Java picks the app per request (`X-Vespera-App`).
vespera::jni_apps! {
    "_default" => create_app,
    "admin"    => admin_app,
}

// The same cdylib also exports the C ABI (`vespera_init` & co.).
vespera::ffi_apps! {
    "_default" => create_app,
    "admin"    => admin_app,
}
```

A single-app library needs only `vespera::jni_app!(create_app);` (and
`features = ["jni"]`).

### Java side

```java
@SpringBootApplication
public class DemoApplication {
    public static void main(String[] args) {
        VesperaBridge.init("rust_jni_demo");
        SpringApplication.run(DemoApplication.class, args);
    }
}
```

> No `@ComponentScan` of `com.devfive.vespera.bridge` — the bridge is
> autoconfigured. Scanning it would pick the controller up as a plain
> `@RestController`, which fails to construct.

### What happens at runtime

1. `vespera::jni_apps!` generates `JNI_OnLoad` → calls `vespera::inprocess::register_app(create_app)` and `register_app_named("admin", admin_app)`
2. Java calls `VesperaBridge.init("rust_jni_demo")` → loads cdylib → triggers `JNI_OnLoad`
3. `VesperaProxyController` catches all HTTP requests → encodes them into the **binary wire format** → the autoconfigured `SmartDispatchModeResolver` (default since vespera-bridge 0.2) picks a dispatch mode per request: `DIRECT` (direct `ByteBuffer`s, `VesperaBridge.dispatchDirect`) for small / bodyless safe requests (GET / HEAD / OPTIONS), `SYNC` (`VesperaBridge.dispatchBytes(byte[])`) for small unsafe ones (POST / PUT / PATCH / DELETE), and the streaming modes for SSE / NDJSON `Accept` or large / unknown-length bodies
4. The JNI symbol delegates to `vespera_inprocess` (e.g. `dispatch_from_bytes()` for `SYNC`)
5. The dispatcher parses the wire header, looks up the cached `Router`, and runs `router.oneshot(request)` with the raw body bytes
6. Response wire bytes flow back the same way; the controller parses status + headers straight from the wire via `WireHeaderReader` and writes the body to the servlet response for every content type (the wire header carries the exact `Content-Type`, written verbatim — no UTF-8 round-trip)
7. No TCP between Java and Rust; **no base64** — multipart uploads, PDFs, images travel as raw bytes

#### Wire format

```
[u32 BE header_len][UTF-8 JSON header][raw body bytes]
```

Header JSON (request and response):

```jsonc
// request
{ "v": 1, "method": "POST", "path": "/upload",
  "query": "user=alice", "headers": {"content-type": "..."} }

// response
{ "v": 1, "status": 200, "headers": {...}, "metadata": {"version": "0.1.51"} }
```

All failure paths (malformed wire, Rust panic, no app registered) return a length-prefixed wire response with `status: 4xx/5xx` and a plain-text body, so the Java decoder never has to special-case errors.

### Maven/Gradle dependency

```kotlin
// build.gradle.kts
repositories {
    mavenCentral()
}
dependencies {
    implementation("kr.devfive:vespera-bridge:0.8.0")
}
```
