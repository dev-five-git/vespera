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
3. `VesperaProxyController` catches all HTTP requests → calls `VesperaBridge.dispatch(json)`
4. JNI symbol delegates to `vespera::inprocess::dispatch_from_json()`
5. `dispatch_from_json` gets the registered factory → builds Router → `router.oneshot(request)`
6. No TCP between Java and Rust

### Maven/Gradle dependency

```kotlin
// build.gradle.kts
repositories {
    maven { url = uri("https://maven.pkg.github.com/dev-five-git/vespera") }
}
dependencies {
    implementation("com.devfive.vespera:vespera-bridge:0.1.0")
}
```
