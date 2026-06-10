# rust-jni-demo

Vespera app that runs in two modes from the same codebase:

| Mode | Transport | How to start |
|------|-----------|--------------|
| **Standalone** | TCP :3000 | `cargo run -p rust-jni-demo` |
| **JNI** | In-process (no network) | Java loads the cdylib |

Both modes use the same `create_app()` → same routes, same logic.

## Prerequisites

- Rust 1.85+
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

### Step 2: Build the vespera-bridge JAR

```bash
cd libs/vespera-bridge
./gradlew jar
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
├── Cargo.toml              # depends on vespera only: features = ["jni"]
├── src/
│   ├── lib.rs              # create_app() + vespera::jni_app!(create_app)
│   ├── main.rs             # Mode A: vespera::axum::serve on :3000
│   └── routes/
│       ├── documents.rs    # POST /documents/validate
│       └── health.rs       # GET /health
└── java/
    └── demo-app/           # Mode B: Spring Boot proxy
        └── src/.../DemoApplication.java
```

## How it works

### Rust side

```rust
// Cargo.toml — single dependency
// vespera = { features = ["jni"] }

// lib.rs — the entire JNI integration:
pub fn create_app() -> axum::Router {
    vespera!(title = "Document Validation API", version = "0.1.0")
}

vespera::jni_app!(create_app);
```

### Java side

```java
@SpringBootApplication
@ComponentScan(basePackages = {"kr.go.demo", "com.devfive.vespera.bridge"})
public class DemoApplication {
    public static void main(String[] args) {
        VesperaBridge.init("rust_jni_demo");
        SpringApplication.run(DemoApplication.class, args);
    }
}
```

### What happens at runtime

1. `vespera::jni_app!` generates `JNI_OnLoad` → calls `vespera::inprocess::register_app(create_app)`
2. Java calls `VesperaBridge.init("rust_jni_demo")` → loads cdylib → triggers `JNI_OnLoad`
3. `VesperaProxyController` catches all HTTP requests → encodes them into the **binary wire format** via `VesperaBridge.encodeRequest(...)` → calls `VesperaBridge.dispatchBytes(byte[])`
4. JNI symbol delegates to `vespera::inprocess::dispatch_from_bytes()`
5. `dispatch_from_bytes` parses the wire header, looks up the cached `Router`, and runs `router.oneshot(request)` with the raw body bytes
6. Response wire bytes flow back the same way; `VesperaBridge.decodeResponse(byte[])` produces a `DecodedResponse` and the controller returns either `ResponseEntity<String>` (text-like Content-Type) or `ResponseEntity<byte[]>` (binary)
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
    maven { url = uri("https://maven.pkg.github.com/dev-five-git/vespera") }
}
dependencies {
    implementation("kr.devfive:vespera-bridge:0.1.1")
}
```
