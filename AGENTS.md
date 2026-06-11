# VESPERA PROJECT KNOWLEDGE BASE

**Generated:** 2026-03-21
**Branch:** main

> This file is the **single source of truth** for repository conventions.
> `CLAUDE.md` is intentionally a one-line redirect (`@AGENTS.md`) — never duplicate
> guidance into CLAUDE.md.

## OVERVIEW

Vespera is a fully automated OpenAPI 3.1 engine for Axum - delivers FastAPI-like DX to Rust. Zero-config route discovery via compile-time macro scanning.

Also provides in-process dispatch (`vespera_inprocess` crate) and JNI integration (`vespera_jni` crate) for embedding Rust axum apps inside Java/Spring applications without HTTP overhead.

### Headline Capabilities (2026)

| Capability | Where | Notes |
|---|---|---|
| **`#[derive(Schema)]` → OpenAPI 3.1** | `vespera_macro::Schema` | Rust types become JSON Schema at compile time, including serde renames, `Option<T>`, `Vec<T>`, SeaORM relations |
| **`Validated<T>` extractor + auto-`422`** | `vespera::Validated`, `crates/vespera/src/validated.rs` | Wraps `Json`/`Form`/`Query`/`Path` and runs `garde::Validate` before the handler — rejection is **`422 Unprocessable Entity`** with `{"errors":[{"path","message"}]}` JSON envelope |
| **`schema_type! { ... }`** | `vespera_macro::schema_type` | Derive request/response DTOs from existing structs (`pick` / `omit` / `partial` / `add` / `multipart` / `omit_default`) — first-class SeaORM relation support |
| **One-liner `.serve(addr)`** | `vespera::Serve` (`crates/vespera/src/serve.rs`) | Extension trait on `axum::Router` — `create_app().serve("0.0.0.0:3000").await` replaces 3 lines of `TcpListener::bind` + `axum::serve` boilerplate |
| **Binary wire format (JNI)** | `vespera_inprocess` | `[u32 BE len | UTF-8 JSON header | raw body]` — multipart / PDFs / images travel as raw bytes; **`422` validation errors hoisted** into the wire header as `"validation_errors": [...]` so Java decoders never special-case error shapes |
| **Multi-app routing (JNI/FFI)** | `vespera::jni_apps! { "_default" => app, "admin" => admin_app }` | Wire header carries optional `"app"` field; Java side picks per request via `X-Vespera-App` header (configurable via `AppNameResolver`) |
| **Zero-config Spring autoconfigure** | `libs/vespera-bridge/.../VesperaBridgeAutoConfiguration` | `VesperaProxyController` + `AppNameResolver` + `DispatchModeResolver` beans auto-registered; replace any of them via `@ConditionalOnMissingBean` |
| **Cron jobs** | `#[vespera::cron("...")]` | Auto-discovered like routes; runs via `tokio-cron-scheduler` |

## STRUCTURE

```
vespera/
├── crates/
│   ├── vespera/              # Public API - re-exports everything
│   │   └── src/lib.rs        # Core re-exports (no transport deps)
│   ├── vespera_core/         # OpenAPI types, route/schema abstractions
│   ├── vespera_macro/        # Proc-macros (main logic lives here)
│   ├── vespera_inprocess/    # In-process dispatch (transport-agnostic)
│   │   └── src/lib.rs        # dispatch(), register_app(), dispatch_from_bytes()
│   └── vespera_jni/          # JNI bridge (depends on vespera_inprocess)
│       └── src/lib.rs        # RUNTIME, jni_app! macro, JNI symbol export
├── libs/
│   └── vespera-bridge/       # Java library (com.devfive.vespera.bridge)
│       ├── VesperaBridge.java          # JNI native loader + dispatch
│       └── VesperaProxyController.java # Auto-configured Spring proxy
├── examples/
│   ├── axum-example/         # Standard axum server demo
│   └── rust-jni-demo/        # JNI + standalone server demo
│       ├── src/              # Rust: routes, create_app(), jni_app!
│       └── java/demo-app/    # Java: Spring Boot proxy
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Add new macro feature | `crates/vespera_macro/src/` | Main macro in `lib.rs` |
| Modify OpenAPI output | `crates/vespera_macro/src/openapi_generator.rs` | JSON generation |
| Add route parser feature | `crates/vespera_macro/src/parser/` | Type extraction logic |
| Change schema generation | `crates/vespera_macro/src/parser/schema.rs` | Rust→JSON Schema |
| Modify route attribute | `crates/vespera_macro/src/args.rs` | `#[route]` parsing |
| Modify schema_type! macro | `crates/vespera_macro/src/schema_macro.rs` | Type derivation & SeaORM support |
| Add core types | `crates/vespera_core/src/` | OpenAPI spec types |
| Test new features | `examples/axum-example/` | Add route, run example |
| In-process dispatch | `crates/vespera_inprocess/src/lib.rs` | RequestEnvelope → Router → ResponseEnvelope |
| App factory (FFI pattern) | `crates/vespera_inprocess/src/lib.rs` | register_app(), dispatch_from_bytes() |
| JNI integration | `crates/vespera_jni/src/lib.rs` | RUNTIME, jni_app! macro, JNI symbol export |
| Java bridge library | `libs/vespera-bridge/` | com.devfive.vespera.bridge package |
| JNI demo (Rust) | `examples/rust-jni-demo/src/` | Routes + vespera::jni_app! |
| JNI demo (Java) | `examples/rust-jni-demo/java/` | Spring Boot proxy app |

## KEY COMPONENTS

| File | Lines | Role |
|------|-------|------|
| `vespera_macro/src/lib.rs` | ~1044 | `vespera!`, `#[route]`, `#[derive(Schema)]` |
| `vespera_macro/src/schema_macro.rs` | ~3000 | `schema_type!` macro, SeaORM relation handling |
| `vespera_macro/src/parser/schema.rs` | ~1527 | Rust struct → JSON Schema conversion |
| `vespera_macro/src/parser/parameters.rs` | ~845 | Extract path/query params from handlers |
| `vespera_macro/src/openapi_generator.rs` | ~808 | OpenAPI doc assembly |
| `vespera_macro/src/collector.rs` | ~707 | Filesystem route scanning |
| `vespera_inprocess/src/lib.rs` | ~1184 | In-process dispatch + app factory + streaming + binary wire |
| `vespera_jni/src/lib.rs` | ~795 | JNI RUNTIME + jni_app! macro + 7 JNI symbols (incl. direct-buffer path) |

## CRATE DEPENDENCY GRAPH

```
vespera (OpenAPI framework)
  ├── vespera_core
  ├── vespera_macro
  ├── vespera_inprocess (optional, feature = "inprocess")
  └── vespera_jni (optional, feature = "jni", implies "inprocess")

vespera_inprocess (transport layer — no JNI deps)
  ├── axum (direct — owns Router re-export)
  ├── bytes (Bytes for zero-copy body handling)
  ├── http, http-body-util, tower
  ├── serde, serde_json
  └── tokio (rt only — for dispatch_from_bytes Runtime param)

vespera_jni (JNI glue — thin layer)
  ├── vespera_inprocess (via workspace)
  ├── jni
  └── tokio (rt-multi-thread — for LazyLock<Runtime>)

rust-jni-demo (example — depends on vespera ONLY)
  └── vespera = { features = ["jni"] }
```

## USER-FACING API

Users depend on `vespera` only. Internal crates are never depended on directly.

```toml
# Cargo.toml — the only dependency needed
[dependencies]
vespera = { version = "...", features = ["jni"] }
```

```rust
// lib.rs — all imports come from vespera
use vespera::{axum, vespera};

pub fn create_app() -> axum::Router {
    vespera!(title = "My API", version = "1.0.0")
}

vespera::jni_app!(create_app);
```

Feature flags:

| Feature | Re-exports | Adds |
|---------|-----------|------|
| `inprocess` | `vespera::inprocess` (= `vespera_inprocess`) | dispatch, register_app, envelopes |
| `jni` | `vespera::jni` (= `vespera_jni`) + implies `inprocess` | RUNTIME, jni_app!, JNI symbol |

## JNI ARCHITECTURE

```
Java (Spring Boot)              Rust (cdylib)           vespera crates
─────────────────              ──────────────          ─────────────────
VesperaBridge.init()       →   JNI_OnLoad             vespera_inprocess::register_app()
    ↓                              ↓
VesperaBridge.dispatchBytes() → JNI symbol            vespera_inprocess::dispatch_from_bytes()
    ↓                              ↓                        ↓
VesperaProxyController         catch_unwind           router.oneshot(request)
    ↓                              ↓                        ↓
ResponseEntity                 binary wire response   axum handlers
   (String OR byte[])          [u32 BE | JSON | body]
```

### Binary Wire Format

Both request and response use the same layout:

```
bytes 0..4    : u32 BE = header_json byte length N
bytes 4..4+N  : UTF-8 JSON
                  (request)  { "v":1, "method", "path",
                               "query"?, "headers"? }
                  (response) { "v":1, "status", "headers",
                               "metadata", "validation_errors"? }
bytes 4+N..   : raw body bytes (UTF-8 text or binary —
                no encoding applied)
```

- No base64 — multipart uploads / PDFs / images travel as raw bytes.
- `"v":1` is the protocol version; mismatched versions get a `400` wire response.
- All failure modes (malformed wire, panic in Rust, no app registered) return a valid length-prefixed wire response, so the Java decoder never has to special-case errors.
- `validation_errors` is an optional array hoisted from 422 JSON bodies (`{"errors":[...]}`) — original body preserved verbatim alongside.

### JNI Dispatch Modes (seven symbols)

| Symbol | Java native | Mode | Memory |
|---|---|---|---|
| `Java_...dispatchBytes` | `byte[] dispatchBytes(byte[])` | sync | full body |
| `Java_...dispatchAsync` | `void dispatchAsync(CompletableFuture<byte[]>, byte[])` | async | full body |
| `Java_...dispatchStreaming` | `byte[] dispatchStreaming(byte[], OutputStream)` | sync response-streaming | chunk-bounded response |
| `Java_...dispatchFullStreaming` | `byte[] dispatchFullStreaming(byte[], InputStream, OutputStream)` | sync bidirectional streaming | chunk-bounded both directions |
| `Java_...dispatchStreamingWithHeader` | `void dispatchStreamingWithHeader(byte[], Consumer<byte[]>, OutputStream)` | sync response-streaming, header callback before first body byte | chunk-bounded response |
| `Java_...dispatchFullStreamingWithHeader` | `void dispatchFullStreamingWithHeader(byte[], Consumer<byte[]>, InputStream, OutputStream)` | sync bidirectional streaming, header callback | chunk-bounded both directions |
| `Java_...dispatchDirect0` | `int dispatchDirect(ByteBuffer, int, ByteBuffer)` (public validated wrapper over the private native) | sync, direct buffers | full body, zero Java heap arrays |

All share the same wire format, registered router, and panic-safe `catch_unwind` discipline. The direct-buffer path (`dispatchDirect` + pooled `dispatchDirectPooled`, per-thread 64 KiB→4 MiB buffers via `vespera.direct.maxBufferBytes`) removes the two JNI region copies of `dispatchBytes`; on response overflow it returns `-(requiredSize)` and a retry **re-runs the handler**, so the Java side only auto-retries idempotent requests (`BufferTooSmallException` otherwise). Spring opt-in via `SmartDispatchModeResolver` → `DispatchMode.DIRECT`; the autoconfigured default remains `BIDIRECTIONAL_STREAMING`. `dispatchAsync` spawns the dispatch on Rust's shared Tokio runtime via `tokio::spawn` (panic → `JoinError` → `error_wire(500)`) and completes the `CompletableFuture` from a worker thread via `attach_current_thread`. `dispatchStreaming` drains the response body chunk-by-chunk via `http_body::Body::frame()` and writes each chunk to the Java `OutputStream`. `dispatchFullStreaming` adds request-side streaming: a `tokio::task::spawn_blocking` thread pulls chunks (default 64 KiB) from `InputStream.read(byte[])` and feeds them into axum via an `mpsc::channel`-backed `http_body::Body`, giving natural backpressure (bounded channel, default 16 slots) so 1 GiB uploads run in `O(chunk_size)` RAM.

**Streaming tuning (process-fixed after first dispatch):** chunk size via system property `vespera.streaming.chunkBytes` / env `VESPERA_STREAMING_CHUNK_BYTES` (default 64 KiB, clamped 4 KiB–8 MiB); channel capacity via `vespera.streaming.channelCapacity` / `VESPERA_STREAMING_CHANNEL_CAPACITY` (default 16, clamped 1–1024). Rust-side setters: `vespera_inprocess::set_streaming_chunk_bytes` / `set_streaming_channel_capacity` (precedence: setter > env > default). The shared Tokio runtime's worker count is tunable the same way: `vespera.runtime.workerThreads` / `VESPERA_RUNTIME_WORKERS` (default: logical CPUs, clamped 1–1024) — cap it when JVM thread pools compete for the same cores. `_default`-app dispatch resolves through a lock-free `OnceLock<Router>` fast path; named apps go through the `RwLock<HashMap>`. The response wire header serializes straight from `http::HeaderMap` (zero per-header allocation) and request wire headers deserialize borrowing from the input buffer (`Cow`) — the wire byte layout is locked by `crates/vespera_inprocess/tests/wire_contract.rs`.

### Rust Public API (vespera_inprocess)

| Function | Sig | Use |
|---|---|---|
| `register_app(F)` | sync | Register the default app (first-wins, BC) |
| `register_app_named(&str, F)` | sync | Register a named app for multi-app routing |
| `dispatch_from_bytes(Vec<u8>, &Runtime) -> Vec<u8>` | sync | FFI entry, blocks on runtime |
| `dispatch_from_bytes_async(Vec<u8>) -> Vec<u8>` (async) | async | inside an existing runtime |
| `dispatch_streaming_async<F>(Vec<u8>, F) -> Vec<u8>` (async) | response streaming async | `F: FnMut(&[u8])` body chunks |
| `dispatch_streaming_with_header_async<H,F>(Vec<u8>, H, F)` (async) | response streaming, header callback first | `H: FnMut(&[u8])` fires before first body chunk |
| `dispatch_bidirectional_streaming<P,F>(Vec<u8>, P, F) -> Vec<u8>` (async) | bidirectional streaming | `P: FnMut() -> Option<Vec<u8>> + Send + 'static`, `F: FnMut(&[u8])` |
| `dispatch_bidirectional_streaming_with_header<P,F,H>(Vec<u8>, P, F, H)` (async) | bidirectional streaming, header callback | header before first body chunk |
| `dispatch_into(Vec<u8>, &mut [u8], &Runtime) -> DirectWriteResult` | sync | direct-write FFI entry — wire response streamed straight into the caller's buffer (no response `Vec`); `Complete(n)` / `Overflow(exact_required)`; 422 materialised internally to keep `validation_errors` hoisting |
| `dispatch_into_async(Vec<u8>, &mut [u8]) -> DirectWriteResult` (async) | async | same, inside an existing runtime |
| `error_wire(u16, &str) -> Vec<u8>` | sync | wire-format error builder |
| `dispatch_typed(Router, &RequestEnvelope) -> ResponseEnvelope` | async | direct axum API (BC) |

### Multi-app routing

**Use case**: multi-app is primarily a feature for **external-dispatcher scenarios** — JNI (Java host picks app per request via header), WebAssembly bridge, C FFI, or any in-process embedding where the host distinguishes between multiple independent vespera API surfaces.  For Rust **standalone** servers (`axum::serve(...)`), the native axum patterns (`Router::merge()`, `Router::nest()`) are more idiomatic for modularization — `register_app_named` adds no value when the same binary owns both the router registration and the HTTP entry point.

The wire header carries an optional `"app": "<name>"` field (default
omitted → `"_default"` app).  Dispatch looks the name up in
`APP_ROUTERS: RwLock<HashMap<String, Router>>` and returns:

- 404 wire response if the name is registered but no such app exists
- 400 wire response if the name fails validation (non-empty, ≤ 64 bytes, `[A-Za-z0-9_-]`)
- Otherwise the matching `Router` is cloned (Arc-backed) and dispatched

Two Rust-side macros assemble the single mandatory `JNI_OnLoad`:

```rust
vespera::jni_app!(create_app);                        // BC sugar for single default app

vespera::jni_apps! {                                  // multi-app primary API
    "_default" => create_app,
    "admin"    => admin_app,
    "public"   => public_app,
}
```

### Spring Boot autoconfigure (Java side)

`vespera-bridge` ships a Spring Boot autoconfiguration that wires up
`VesperaProxyController` + two strategy beans, both replaceable via
`@ConditionalOnMissingBean`:

- `AppNameResolver` (default: `HeaderAppNameResolver("X-Vespera-App")`) — picks app per request
- `DispatchModeResolver` (default: `BidirectionalStreamingDispatchModeResolver`) — picks `DispatchMode`

Property `vespera.bridge.controller-enabled=false` disables the whole controller for BYO scenarios.  See [`libs/vespera-bridge/README.md`](libs/vespera-bridge/README.md#customization) for the customization recipes.

### Rust side (example app — 2 lines of JNI code):
```rust
pub fn create_app() -> axum::Router { vespera!(...) }
vespera::jni_app!(create_app);
```

### Java side (user app — 1 meaningful line):
```java
VesperaBridge.init("rust_jni_demo");
SpringApplication.run(DemoApplication.class, args);
```

## SCHEMA_TYPE! MACRO

Generate request/response types from existing structs with powerful transformations.

### Key Features
- **Same-file Model reference**: `schema_type!(Schema from Model, name = "UserSchema")`
- **Cross-file reference**: `schema_type!(Response from crate::models::user::Model, omit = ["password"])`
- **SeaORM integration**: Automatic conversion of `HasOne`, `BelongsTo`, `HasMany` relations
- **Chrono conversion**: `DateTimeWithTimeZone` → `vespera::chrono::DateTime<FixedOffset>`
- **Circular reference handling**: Automatic detection and inline field generation

### Parameters
| Parameter | Description |
|-----------|-------------|
| `pick` | Include only specified fields |
| `omit` | Exclude specified fields |
| `rename` | Rename fields: `[("old", "new")]` |
| `add` | Add new fields (disables auto `From`) |
| `clone` | Control Clone derive (default: true) |
| `partial` | Make fields optional for PATCH |
| `name` | Custom OpenAPI schema name |
| `rename_all` | Serde rename strategy |
| `ignore` | Skip Schema derive |

## REPOSITORY SHAPE

Vespera is a **hybrid monorepo** with two workspaces co-located at the repo root:

| Workspace | Manager | Members | Purpose |
|---|---|---|---|
| Cargo (`Cargo.toml`) | cargo | `crates/*`, `examples/*` (excluding `examples/java-jni-demo`) | OpenAPI engine, proc-macros, JNI bridge |
| Bun (`package.json`) | bun | `apps/*` | Marketing/docs site + admin panel (Next.js) |

`bun run ...` operates on the Node side; `cargo ...` on the Rust side. Many root
scripts deliberately cross the boundary — e.g., `prelint` runs `cargo
clippy/fmt/check` **before** oxlint touches JS.

### Common Commands

```bash
# --- Rust side ---
cargo build                           # Build all crates
cargo test --workspace                # All Rust tests
cargo test -p vespera_macro           # One crate
cargo test --test <name> -- <filter>  # Single integration test
cargo tarpaulin --out stdout          # Coverage (via `bun run posttest`)

# --- Lint / format (order matters — `prelint` runs Rust FIRST) ---
bun run lint                          # oxlint (after `cargo clippy + fmt --check + check`)
bun run lint:fix                      # oxlint --fix (after `cargo clippy --fix && cargo fmt`)

# --- Front-end workspace ---
bun run dev                           # `dev` in every apps/*
bun run build                         # apps/front + apps/admin
cd apps/front && bun dev              # Single-app dev (preferred per devfive-frontend)

# --- Tests (Bun side) ---
bun test                              # Root runs bun test + tarpaulin (posttest hook)

# --- Release tooling ---
bun run changepacks                   # @changepacks/cli version bumps
```

> **`prelint` gotcha:** any Rust warning fails the JS lint. Run `bun run
> lint:fix` to auto-resolve both sides.

### Frontend (`apps/front`)

Next.js 16 App Router + React 19 + `@devup-ui/react` (build-time CSS-in-JS).
Theme tokens live in `apps/front/devup.json` and use `$token` syntax in JSX
props only.

- `apps/front/src/app/` contains **only** `layout.tsx` + `page.tsx` — all other
  components live in `src/components/` (per devfive-frontend conventions).
- Styling uses devup-ui shorthand props (`bg`, `p`, `w`, `_hover`,
  `[mobile,null,pc]` responsive arrays). Never `style={{...}}` or Tailwind.

### Where Tests Live

| Concern | Location |
|---|---|
| Macro integration tests | `crates/vespera_macro/tests/` (+ `insta` snapshots) |
| Validated/422 contract | `crates/vespera/tests/validated_extractor.rs`, `crates/vespera/tests/jni_validation.rs` |
| Core unit tests | `crates/vespera_core/src/**` inline `#[cfg(test)]` |
| JNI end-to-end | `examples/rust-jni-demo` (Rust + Java + Gradle) |
| Front tests | `apps/front/src/__tests__/` (`bun test` + `bun-test-env-dom`) |

`insta` snapshots — run `cargo insta review` to accept drifts.

### Pre-Commit (Husky)

`bun run prepare` installs husky; commits trigger `.husky/` hooks (typically
`lint`). Never bypass with `--no-verify`; fix the underlying finding.

## CONVENTIONS

- **File size cap**: every source file stays ≤ 1000 lines. Unit tests live **inline** (`#[cfg(test)] mod tests`) whenever code + tests fit the cap; only when they don't, tests move to sidecar child modules (`<module>/tests.rs`, `<module>/tests_<topic>.rs` — `use super::*` semantics preserved). Token-stream assertions use rstest cases + insta snapshots (explicit per-case snapshot names; `prettyplease` for item output) instead of `contains` probes.
- **Rust 2024 edition** across all crates
- **Workspace dependencies**: Internal crates use `{ workspace = true }`
- **Test frameworks**: `rstest` for unit tests, `insta` for snapshots
- **No `build.rs`**: All code gen via proc-macros at compile time
- **No direct axum dep in examples**: Use `vespera::axum` re-export
- **No direct vespera_jni/vespera_inprocess dep**: Use `vespera` features
- **Java package**: `com.devfive.vespera.bridge` (fixed for JNI symbol stability)
- **Java build**: Gradle (Kotlin DSL), published to GitHub Packages

## ANTI-PATTERNS (THIS PROJECT)

- **NEVER** add `build.rs` - macro handles compile-time generation
- **NEVER** manually register routes - `vespera!` macro discovers them
- **NEVER** write OpenAPI JSON by hand - generated from code
- **NEVER** write JNI boilerplate in examples - use `vespera::jni_app!` macro
- **NEVER** parse domain JSON in Java - Spring is a proxy, Rust owns business logic
- **NEVER** depend on axum directly in examples - use `vespera::axum`
- **NEVER** depend on `vespera_jni` or `vespera_inprocess` directly - use `vespera` features
- **NEVER** put transport logic in vespera core - use `vespera_inprocess` / `vespera_jni`
- Route functions **MUST** be `pub async fn`

## COMMANDS

```bash
# Development
cargo build                    # Build all crates
cargo test --workspace         # Run all tests
cargo test -p vespera_macro    # Test macros only
cargo test -p rust-jni-demo    # Test JNI demo

# Run axum example
cd examples/axum-example
cargo run                      # Starts server on :3000

# Run JNI demo (standalone Rust server)
cargo run -p rust-jni-demo     # Starts server on :3000

# Run JNI demo (Java + Rust)
cd libs/vespera-bridge && ./gradlew jar
cargo build -p rust-jni-demo --release
cd examples/rust-jni-demo/java && ./gradlew :demo-app:bootJar
java -jar demo-app/build/libs/demo-app-0.1.0.jar

# Check generated OpenAPI
cat examples/axum-example/openapi.json
```

## NOTES

- Macro performs **filesystem I/O at compile time** - may affect IDE performance
- OpenAPI files are **regenerated on every build** when `openapi = "..."` specified
- `CARGO_MANIFEST_DIR` env var used to locate `src/routes/` folder
- Generic types in schemas require `#[derive(Schema)]` on all type params
- JNI native library can be bundled inside the fat JAR for single-file deployment
- `VesperaBridge.init()` auto-extracts bundled native lib to temp, falls back to system path
