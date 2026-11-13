# VESPERA PROJECT KNOWLEDGE BASE

**Generated:** 2026-03-21
**Branch:** main

> This file is the **single source of truth** for repository conventions.
> `CLAUDE.md` is intentionally a one-line redirect (`@AGENTS.md`) — never duplicate
> guidance into CLAUDE.md.

## OVERVIEW

Vespera is a fully automated OpenAPI 3.1 engine for Axum - delivers FastAPI-like DX to Rust. Zero-config route discovery via compile-time macro scanning.

Also provides in-process dispatch (`vespera_inprocess` crate) and JNI integration (`vespera_jni` crate) for embedding Rust axum apps inside Java/Spring applications without HTTP overhead, plus a stable C ABI (`vespera_inprocess::ffi`, feature `ffi`) for non-JVM hosts (C/C++, Go, Python ctypes, .NET, Node).

### Headline Capabilities (2026)

| Capability | Where | Notes |
|---|---|---|
| **`#[derive(Schema)]` → OpenAPI 3.1** | `vespera_macro::Schema` | Rust types become JSON Schema at compile time, including serde renames, `Option<T>` as `type:[...,"null"]` / nullable `$ref` as `anyOf`, schema-level `examples:[...]` (not singular `example`), `Vec<T>`, SeaORM relations |
| **`Validated<T>` extractor + auto-`422`** | `vespera::Validated`, `crates/vespera/src/validated.rs` | Wraps `Json`/`Form`/`Query`/`Path` and runs `garde::Validate` before the handler — rejection is **`422 Unprocessable Entity`** with `{"errors":[{"path","message"}]}` JSON envelope |
| **OAuth2 / OIDC scopes** | `vespera!(security_schemes = [{ type = "oauth2", flows = {...} }])`, `#[route(security = [("oauth2", ["read:users"])])]` | Flows (`implicit` / `password` / `client_credentials` / `authorization_code` + `scopes`) and `openIdConnect` emit `components.securitySchemes`; requirements may carry scopes, AND groups (`[["apiKey", ("oauth2", [...])]]`) and an optional-auth `[]`. Compile errors for unknown schemes and for scopes no flow declares (`vespera_macro/src/security.rs` + `router_codegen/security_dsl.rs`); runtime `Principal::require_scopes` → `403` `insufficient_scope` |
| **`schema_type! { ... }`** | `vespera_macro::schema_type` | Derive request/response DTOs from existing structs (`pick` / `omit` / `partial` / `add` / `multipart` / `omit_default`) — first-class SeaORM relation support |
| **One-liner `.serve(addr)`** | `vespera::Serve` (`crates/vespera/src/serve.rs`) | Extension trait on `axum::Router` — `create_app().serve("0.0.0.0:3000").await` replaces 3 lines of `TcpListener::bind` + `axum::serve` boilerplate |
| **Binary wire format (JNI)** | `vespera_inprocess` | `[u32 BE len | UTF-8 JSON header | raw body]` — multipart / PDFs / images travel as raw bytes; **`422` validation errors hoisted** into the wire header as `"validation_errors": [...]` so Java decoders never special-case error shapes |
| **Multi-app routing (JNI/FFI)** | `vespera::jni_apps! { "_default" => app, "admin" => admin_app }` | Wire header carries optional `"app"` field; Java side picks per request via `X-Vespera-App` header (configurable via `AppNameResolver`) |
| **Zero-config Spring autoconfigure** | `libs/vespera-bridge/.../VesperaBridgeAutoConfiguration` | `VesperaProxyController` + `AppNameResolver` + `DispatchModeResolver` + `PrincipalResolver` beans auto-registered; replace any of them via `@ConditionalOnMissingBean` |
| **WebFlux proxy** | `libs/vespera-bridge/.../VesperaBridgeReactiveAutoConfiguration` | REACTIVE web apps (WebFlux compile-only, never transitive): `VesperaReactiveProxyHandler` dispatches via `dispatchAsync` (no event-loop blocking; Mono cancel → future cancel → native `cancel0`), aggregated body capped by `max-buffered-request-bytes` (`413`), `text/event-stream`/NDJSON `Accept` streamed through `dispatchStreamingWithHeader` on `boundedElastic`; reuses `HeaderPolicy`, app allowlist, principal (`exchange.getPrincipal()`), include/exclude patterns, `vespera.bridge.*`, observers/metrics |
| **GraalVM native image** | `libs/vespera-bridge/src/main/resources/META-INF/native-image/kr.devfive/vespera-bridge/` | Bridge JAR ships `jni-config.json` (every JNI class/method/`ThrowNew` exception `crates/vespera_jni` + the `jni` crate's internal bindings look up), `resource-config.json` (`native/<platform>/<lib>`, `vespera-bridge.properties`), `reflect-config.json` (`Thread.isVirtual`); Spring AOT via `@ImportRuntimeHints`: `VesperaBridgeRuntimeHints` (servlet autoconfiguration only — proxy method) + servlet-free `VesperaSecurityRuntimeHints` (both servlet and WebFlux autoconfigurations — Spring Security authorities; the anonymous-token check compares class names, so it needs no hint and fails closed). `NativeImageMetadataTest` scans the Rust sources so a new JNI lookup without metadata fails `./gradlew test`; demo-app opt-in `-PnativeImage :demo-app:nativeCompile` + `scripts/native-image-smoke.sh`; manual `native-image.yml` workflow |
| **Programmatic Java client** | `libs/vespera-bridge/.../VesperaClient` | `VesperaClient.create().get("/users/42").send()` → `VesperaResponse` (status / headers / raw body / hoisted `422` errors) in-process, no HTTP; SYNC / ASYNC / streamed-upload paths; autoconfigured bean independent of `controller-enabled` |
| **C ABI for non-JVM hosts** | `vespera::ffi_app!` / `ffi_apps!` + `vespera_inprocess::ffi` (feature `ffi`) | `vespera_init` / `vespera_dispatch` / `vespera_dispatch_into` / `vespera_dispatch_async` / `vespera_free` / `vespera_shutdown` / `vespera_abi_version`; hand-written header `crates/vespera_inprocess/include/vespera.h` (locked by `tests/ffi_header.rs`); same wire format + shared runtime as JNI |
| **Cron jobs** | `#[vespera::cron("...")]` | Auto-discovered like routes (`CRON_STORAGE` + a module-tree scan fallback in `vespera_impl/cron_discovery.rs` for modules expanded after `vespera!`); started once per `vespera!` invocation via `vespera::cron::start_once` — on the current Tokio runtime; in a host-loaded library (`JNI_OnLoad`, or `vespera_init` from `ffi_app!`) on the shared host runtime via `vespera_inprocess::runtime::on_shared_runtime` (`host_loaded()`) (honours `configureRuntime`, stops on `shutdown0`; `VesperaBridge.init()` starts the runtime when jobs wait); otherwise a dedicated `vespera-cron` thread (plain `#[test]`); jobs sleep until the wall clock reaches the occurrence; croner 4, shared parser in `vespera_core::cron`. **UTC by default**; `tz = "Asia/Seoul"` needs the opt-in `cron-tz` feature (chrono-tz; name checked at compile time, `tz` without the feature is a compile error); `overlap = "allow"` (default) / `"skip"`; every run is supervised (panics logged via `eprintln!`, schedule continues); `cron::shutdown()` / `shutdown_and_wait(timeout)` stop scheduling process-wide and for good, and `serve_with_graceful_shutdown` calls both (10 s grace) |
| **Per-platform native jars** | Gradle plugin `vespera { nativeJarPlatforms; nativeJarsDir; bundleNative }` (`libs/vespera-bridge-gradle-plugin/.../NativeJars.kt`) | One classifier jar per platform (`myapp-native-0.1.0-linux-x86_64.jar` = only `native/<platform>/<lib>`, packaged from CI-matrix artifacts) + a `vesperaNative` `maven-publish` publication; consumers `runtimeOnly("<g>:<a>:<v>:<platform>")` with `bundleNative=false`; loader errors list the platforms the classpath bundles vs the host (`BundledNativeIndex`); user CI template `.github/workflow-templates/vespera-native-jars.yml` (not used by vespera's own CI) |
| **Kotlin extensions** | `libs/vespera-bridge/src/main/kotlin/.../VesperaKotlin.kt` | Inside the same `vespera-bridge` JAR (no extra artifact): `suspend fun VesperaRequest.sendAwait()` (coroutine cancellation cancels the future → native cancel-on-disconnect), `client.get(path) { header(..); query(..) }` receiver DSL, `Pair` vararg `query`/`headers`. kotlin-stdlib / kotlinx-coroutines are `compileOnly` (`kotlin.stdlib.default.dependency=false`) → POM stays empty; JSpecify `@NullMarked`/`@Nullable` (compileOnly) on the client API |

## STRUCTURE

```
vespera/
├── crates/
│   ├── vespera/              # Public API - re-exports everything
│   │   └── src/lib.rs        # Core re-exports (no transport deps)
│   ├── vespera_core/         # OpenAPI types, route/schema abstractions
│   ├── vespera_macro/        # Proc-macros (main logic lives here)
│   ├── vespera_inprocess/    # In-process dispatch (transport-agnostic)
│   │   ├── src/lib.rs        # dispatch(), register_app(), dispatch_from_bytes()
│   │   ├── src/wire/         # hand-rolled wire-header parse (header_read) + serialize (header_write)
│   │   ├── src/runtime.rs    # shared host runtime (feature `runtime`) used by JNI + C ABI
│   │   ├── src/ffi.rs        # stable C ABI (feature `ffi`) + ffi_app!/ffi_apps!
│   │   └── include/vespera.h # hand-written C header for the ffi symbols
│   └── vespera_jni/          # JNI bridge (depends on vespera_inprocess)
│       ├── src/jni_impl.rs   # RUNTIME, jni_app! macro, JNI symbol export
│       └── src/streaming_closures.rs  # Streaming closure factories + JMethodID cache
├── libs/
│   └── vespera-bridge/       # Java library (com.devfive.vespera.bridge)
│       ├── VesperaBridge.java          # Public bridge facade + dispatch helpers
│       ├── VesperaNativeLoader.java    # Native library extraction/loading
│       ├── WireHeaderStringSupport.java # Wire-header JSON/string helpers
│       ├── RequestShape.java           # Request body/idempotency classifier
│       └── VesperaProxyController.java # Auto-configured Spring proxy
├── examples/
│   ├── axum-example/         # Standard axum server demo
│   ├── testkit/              # Tiny in-process test client (tower oneshot) for the examples' integration tests
│   └── rust-jni-demo/        # JNI + standalone server demo
│       ├── src/              # Rust: routes, create_app(), jni_app!
│       └── java/demo-app/    # Java: Spring Boot proxy (servlet)
│       └── java/demo-reactive-app/ # Java: Spring WebFlux proxy E2E (no servlet API)
```

## WHERE TO LOOK

| Task | Location | Notes |
|------|----------|-------|
| Add new macro feature | `crates/vespera_macro/src/` | Main macro in `lib.rs` |
| Modify OpenAPI output | `crates/vespera_macro/src/openapi_generator.rs` | JSON generation |
| Add route parser feature | `crates/vespera_macro/src/parser/` | Type extraction logic |
| Change schema generation | `crates/vespera_macro/src/parser/schema/` | Rust→JSON Schema |
| Modify route attribute | `crates/vespera_macro/src/args.rs` | `#[route]` parsing |
| Modify schema_type! macro | `crates/vespera_macro/src/schema_macro/` | Type derivation & SeaORM support |
| Add core types | `crates/vespera_core/src/` | OpenAPI spec types |
| Test new features | `examples/axum-example/` | Add route, run example |
| In-process dispatch | `crates/vespera_inprocess/src/dispatch.rs` | RequestEnvelope → Router → ResponseEnvelope; wire + direct-write entry points |
| Wire header parse/serialize | `crates/vespera_inprocess/src/wire/` | Hand-rolled `header_read` (request parse) + `header_write` (response serialize); byte-identical to serde_json, whose twins are kept private (`*_serde`) for the criterion A/B |
| App factory (FFI pattern) | `crates/vespera_inprocess/src/registry.rs` | register_app(), resolve_app_router() |
| JNI integration | `crates/vespera_jni/src/jni_impl.rs` | RUNTIME, jni_app! macro, JNI symbol export |
| C ABI (non-JVM hosts) | `crates/vespera_inprocess/src/ffi.rs` + `include/vespera.h` | `extern "C"` symbols, `ffi_app!`/`ffi_apps!`; keep the header and `tests/ffi_header.rs` in sync |
| Shared host runtime | `crates/vespera_inprocess/src/runtime.rs` | lazy build, leases, hooks, shutdown (JNI re-exports it) |
| Java bridge library | `libs/vespera-bridge/` | com.devfive.vespera.bridge package |
| JNI demo (Rust) | `examples/rust-jni-demo/src/` | Routes + vespera::jni_app! |
| JNI demo (Java) | `examples/rust-jni-demo/java/` | Spring Boot proxy app |

## KEY COMPONENTS

> Paths are repository-relative so each row is directly `Test-Path`-able; line
> counts are approximate and drift between refreshes.

| File | Lines | Role |
|------|-------|------|
| `crates/vespera_macro/src/lib.rs` | ~420 | Proc-macro entry points: `vespera!`, `export_app!`, `#[route]`, `#[derive(Schema)]` |
| `crates/vespera_macro/src/schema_macro/` | split modules | `schema_type!` macro, SeaORM relation handling |
| `crates/vespera_macro/src/parser/schema/` | split modules | Rust struct/enum/type → JSON Schema conversion |
| `crates/vespera_macro/src/parser/parameters.rs` | ~195 | Extract path/query/header params from handlers |
| `crates/vespera_macro/src/openapi_generator.rs` | ~649 | OpenAPI doc assembly |
| `crates/vespera_macro/src/collector.rs` | ~345 | Filesystem route scanning |
| `crates/vespera_inprocess/src/lib.rs` | ~125 | Crate root: module wiring + public re-exports + `#[doc(hidden)]` `bench_support` (modularized — logic lives in the files below) |
| `crates/vespera_inprocess/src/wire.rs` | ~605 | Binary wire frame split/parse + 422 validation-error hoisting; `parse_wire_header` / `write_wire_header_into{,_slice}` delegate to the hand-rolled `wire/` submodules (serde_json twins retained private as `*_serde` for the criterion A/B) |
| `crates/vespera_inprocess/src/wire/header_read.rs` | ~811 | Hand-rolled request-header JSON reader → `WireRequestHeader<'a>`: borrow-when-plain / own-when-escaped `Cow`, UTF-16 surrogate decode, any key order + unknown-skip + dup-reject, never panics (byte-behaviour-identical to the serde derive) |
| `crates/vespera_inprocess/src/wire/header_write.rs` | ~596 | Hand-rolled response-header JSON serializer: `serde_json`-exact escape table + `\u00XX`, sorted `HeaderMap`, metadata, `validation_errors`; one `JsonSink` serves the `Vec` and overflow-counting `&mut [u8]` paths (byte-identical to `serde_json`) |
| `crates/vespera_inprocess/src/dispatch.rs` | ~866 | Public dispatch entry points: text envelope API, binary wire API, direct-write (`dispatch_into`) API |
| `crates/vespera_inprocess/src/internal.rs` | ~828 | Request building + router oneshot + response collection (malformed path/header → 400) |
| `crates/vespera_inprocess/src/streaming.rs` | ~939 | Response / header-callback / bidirectional streaming; `RequestChunk`/`StreamAbort` error-aware request body; bounded `ChannelBody` |
| `crates/vespera_inprocess/src/streaming/producer.rs` | ~500 | `ChannelBody` (bounded mpsc request body), lazily spawned `spawn_blocking` producer + producer-slot semaphore, `RequestSourceCloser`, bounded producer join |
| `crates/vespera_inprocess/src/streaming/inline_pull.rs` | ~137 | Small-body fast path: on a `block_on` caller thread (no task id), no dispatch timeout, before the handler returns — the first 2 request pulls run inline, so a one-chunk body never starts the producer |
| `crates/vespera_inprocess/src/obs.rs` | ~150 | Feature `tracing` only: `instrument` (span + per-poll panic guard + duration), `record_request` / `record_status` (current `vespera.dispatch` span only), `error_response` (event per synthesised error status, hooked into `error_wire`) |
| `crates/vespera_inprocess/src/registry.rs` | ~298 | App registration + lock-free default-app `OnceLock` + lock-free named-app `ArcSwap<HashMap<String, Router>>` (copy-on-write `rcu` on register) |
| `crates/vespera_inprocess/benches/dispatch/serde_ab.rs` | ~271 | Criterion A/B helpers comparing serde_json vs hand-rolled wire-header paths |
| `crates/vespera_jni/src/lib.rs` | ~147 | Crate root: module wiring, `jni` / `vespera_inprocess` re-exports, optional mimalloc `#[global_allocator]` |
| `crates/vespera_jni/src/jni_impl.rs` | ~811 | JNI RUNTIME + `jni_app!` macro + the sync/async/streaming `Java_...` symbols; shared helpers the sibling `jni_impl_*` modules import |
| `crates/vespera_inprocess/src/direct.rs` | ~420 | DIRECT write: `finish_resumable` (never drains past `out`; `text/event-stream` handed over), `PendingDirectResponse` (`write_into` / `stream`, deadline-bounded), legacy exact-size `finish_direct_write` (overflow count capped by `max_buffered_response_bytes`) |
| `crates/vespera_jni/src/jni_impl_direct.rs` | ~330 | `dispatchDirect0` symbol + its `jint` overflow/unrepresentable sentinel protocol; `run_direct_symbol` shared buffer validation |
| `crates/vespera_jni/src/jni_impl_park.rs` | ~135 | `AsyncDispatch.dispatchPollFirst0` (virtual-thread `ParkingDispatch`): poll the dispatch once on the caller under the runtime context (no-op waker); ready → inline wire, pending → spawn the half-polled future, a worker completes the `CompletableFuture`; holds the async in-flight permit + `RuntimeLease` throughout
| `crates/vespera_jni/src/jni_impl_direct_stash.rs` | ~290 | DIRECT overflow stash (`BTreeMap`, at most 1024 entries, 30 s TTL) + `DirectStash.{dispatchResumable0,fetch0,discard0}`; handle = 8-byte BE token + flags byte (`FLAG_EXACT`, `FLAG_STREAM`) in `out[0..9]` |
| `crates/vespera_inprocess/src/runtime.rs` | ~400 | Feature `runtime`: the shared host runtime for JNI + C ABI — worker / blocking-pool knobs, lazy build, `RuntimeLease` in-flight counting through `LeaseGate` (count + shut-down flag in one atomic word), graceful `shutdown_runtime` / `shutdown_runtime_then(timeout, cleanup)` (drain, then `shutdown_timeout`; background thread when called inside a runtime), `on_shared_runtime` hooks (cron), `mark_host_loaded` / `host_loaded`; `vespera_jni` re-exports it (its `shutdown_runtime` adds the DIRECT-stash cleanup) |
| `crates/vespera_inprocess/src/runtime/gate.rs` | ~200 | Tokio-free `LeaseGate` (runtime lease count + shut-down bit, one `fetch_add` to enter, `fetch_or` to close); atomics swap to loom under `--cfg vespera_loom` (unit-test build only) and its `loom_*` models live here |
| `crates/vespera_jni/src/jni_impl_gates.rs` | ~140 | JNI-free `InFlightCap` (`dispatchAsync` cap) admission protocol; atomics swap to loom under `--cfg vespera_loom` and its `loom_*` model lives here |
| `crates/vespera_inprocess/src/ffi.rs` | ~500 | Feature `ffi`: stable C ABI — `vespera_dispatch` (Rust-allocated `Box<[u8]>` out, `vespera_free`), `vespera_dispatch_into` (`VESPERA_OVERFLOW` + required size; overlapping buffers rejected), `vespera_dispatch_async` (callback exactly once; `503` from `Drop` when the task is dropped at shutdown), `vespera_shutdown`, `vespera_abi_version`, `ffi_app!` / `ffi_apps!` (emit `vespera_init` → `__init`); every entry point `catch_unwind`-guarded; header `include/vespera.h` (`ffi::HEADER`) |
| `crates/vespera_jni/src/jni_impl_streaming_header.rs` | ~489 | The two `*WithHeader` streaming symbols (response-only and bidirectional) |
| `crates/vespera_jni/src/jni_impl_support.rs` | ~243 | Per-symbol setup routines + abort/panic-policy helpers shared by the streaming symbols |
| `crates/vespera_jni/src/jni_impl_streaming_buffer.rs` | ~199 | Per-thread reusable Java chunk `byte[]` pool + lease (a panicking dispatch keeps the lease checked out); the TLS destructor makes no JNI call — it parks the `Global` array in a graveyard freed on the next attached checkout (deliberately leaked if even that fails) |
| `crates/vespera_jni/src/jni_impl_config.rs` | ~140 | `configure*0` JNI hooks (runtime workers + max blocking threads, streaming chunk/channel, request / streaming-request / buffered-response limits, dispatch timeout, async in-flight cap) applied before the first dispatch; each returns `jboolean` (false = too late / not applied) |
| `crates/vespera_jni/src/jni_buf.rs` | ~215 | Zero-fill-free `GetByteArrayRegion` read into a `Vec<u8>`'s uninitialised spare capacity; the JNI-free `fill_spare` (reserve → raw fill → `set_len`) and `as_jbytes` (`u8`→`jbyte` view) halves are Miri-tested |
| `crates/vespera_jni/src/streaming_closures.rs` | ~762 | Streaming closure factories (`make_pull_closure`, `make_push_closure`, `call_header_consumer`, `complete_future`) + `OnceLock<MethodCache>` caching `JMethodID`+`GlobalRef<JClass>` for `InputStream.read`, `OutputStream.write`, `Consumer.accept`, `CompletableFuture.complete` — `call_method_unchecked` on the hot path. Pull/push/header closures attach via [`daemon_env::with_cached_daemon_env`] (TLS-cached daemon attach), not `attach_current_thread` per chunk |
| `crates/vespera_jni/src/daemon_env.rs` | ~261 | `with_cached_daemon_env(jvm, cb)` — resolves the current OS thread's `JNIEnv` via `GetEnv` on **every** call (a borrowed env is never cached — a host thread may detach/reattach) and only remembers its **own** daemon attachment in a `thread_local!`: unattached Tokio/`spawn_blocking` threads are attached once via `AttachCurrentThreadAsDaemon` and detached in the TLS `Drop` on thread exit; already-attached JVM threads are borrowed and never detached. Used for every JNI callback (streaming chunk pull/push, header callbacks, async `CompletableFuture.complete`); per-call local frame + exception scrub preserved |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/WireHeaderReader.java` | ~984 | Zero-copy absolute-index reader applying wire-header status/headers straight from a pooled direct `ByteBuffer` |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaProxyController.java` | ~664 | Catch-all Spring proxy: servlet request → wire bytes → chosen `DispatchMode` → `ResponseEntity` |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaBridge.java` | ~920 | Public bridge facade: `native` methods bound to the fixed JNI symbol names, library load, dispatch API |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaWireCodec.java` | ~770 | Pure-Java wire request encoding + response decoding (`DecodedResponse`) |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaDirectBufferPool.java` | ~558 | Per-thread direct `ByteBuffer` pool, adaptive retention, overflow-retry policy behind `dispatchDirectPooled` |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/HeaderPolicy.java` | ~392 | Hop-by-hop / proxy-owned header filtering for request and response forwarding |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaBridgeAutoConfiguration.java` | ~235 | Spring Boot autoconfigure: controller + `AppNameResolver` + `DispatchModeResolver` beans, all `@ConditionalOnMissingBean` |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaBridgeReactiveAutoConfiguration.java` | ~150 | WebFlux autoconfigure (`REACTIVE` + `DispatcherHandler`/Reactor on classpath): `ReactiveAppNameResolver`, `ReactivePrincipalResolver`, `VesperaReactiveProxyHandler`, `VesperaReactiveHandlerMapping` (order `LOWEST_PRECEDENCE - 10`), `VesperaReactivePreflightFilter` (`cors=rust`), all `@ConditionalOnMissingBean` |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaReactiveProxyHandler.java` | ~480 | WebFlux `WebHandler`: aggregate body (`DataBufferUtils.join`, `413`) → wire → `dispatchAsync` (`doOnCancel` cancels the future; observers see `499`) or header-first streaming via `outputStreamPublisher` on `boundedElastic`; package-private `Transport` seam for tests |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/ReactiveProxyResponse.java` / `ReactiveHeaderSource.java` | ~150 / ~155 | Reactive response writer (wire header → `ServerHttpResponse`, CORS / request-id ownership, zero-copy body) and request header/`remote_addr`/`scheme` source; servlet-free (verified by `ReactiveNoServletClasspathTest`) |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaClient.java` | ~215 | Programmatic in-process client facade (`create()` / `forApp(name)` / `get`/`post`/...): no HTTP, no proxy; package-private `Transport` seam (native impl checks `isInitialized`; unit tests fake it) |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaRequest.java` | ~445 | Client request builder: percent-encoded query, lower-cased headers, raw json/text/form/bytes/`InputStream` bodies, app, `remote_addr`/`scheme`; `send()` SYNC (`ParkingDispatch`), `sendAsync()` ASYNC, `InputStream` body → `dispatchFullStreaming`; encodes via `VesperaWireCodec` |
| `libs/vespera-bridge/src/main/kotlin/com/devfive/vespera/bridge/VesperaKotlin.kt` | ~95 | Kotlin client extensions (`sendAwait` via `kotlinx.coroutines.future.await`, request DSL); compiled at language/API 2.0 (`-Xsuppress-version-warnings`) so Kotlin 1.9+ consumers read it; tests in `src/test/kotlin` over the fake `Transport`; `NoSpringClasspathTest` proves the Java client runs without Kotlin on the classpath |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaResponse.java` | ~180 | Client response: status, headers (case-insensitive), body bytes/string (content-type charset), hoisted `validationErrors()`, `panicked()`; never parses domain JSON. Transport failures → `VesperaClientException` (statuses are responses) |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaBridgeProperties.java` | ~185 | `vespera.bridge.*` configuration properties backing the autoconfigured defaults |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaSpringdocAutoConfiguration.java` | ~112 | Optional springdoc-openapi integration (compile-only springdoc dep): Rust `spec_url` documents fetched in-process via `VesperaOpenApiSource`; `group` mode = per-app Swagger UI definition at `/v3/api-docs/vespera/<app>` (`VesperaOpenApiDocsController` + `VesperaSwaggerUiUrls`), `merge` mode = `VesperaOpenApiMerger` `OpenApiCustomizer`; props `vespera.bridge.springdoc.*` (`VesperaSpringdocProperties`) |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/SmartDispatchModeResolver.java` | ~161 | Autoconfigured default `DispatchModeResolver` choosing DIRECT / SYNC / streaming per request |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/WireHeaderStringSupport.java` | ~114 | Shared Java helpers for wire-header UTF-8 strings and JSON escaping |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaNativeLoader.java` | ~720 | Native library lookup/extraction/loading for the bridge facade; `loadFromSystemPath` combines a bundled miss + `java.library.path` failure into one error |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/BundledNativeIndex.java` | ~155 | Failure-path diagnostics: lists `native/<platform>/<file>` across classpath jars/dirs (+ known-platform probes) so a missing library names the platforms found vs the host |
| `libs/vespera-bridge-gradle-plugin/src/main/kotlin/kr/devfive/vespera/NativeJars.kt` | ~100 | `nativeJar<Platform>` classifier jar tasks + `nativeJars` + the `vesperaNative` Maven publication |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/VesperaBridgeRuntimeHints.java` / `VesperaSecurityRuntimeHints.java` | ~30 / ~50 | Spring AOT `RuntimeHintsRegistrar`s (native image): servlet-only `VesperaProxyController#proxy`; servlet-free Spring Security `getAuthorities`/`getAuthority` when present (imported by both the servlet and the WebFlux autoconfiguration); JNI/resources live in the static `META-INF/native-image/kr.devfive/vespera-bridge/*-config.json` |
| `libs/vespera-bridge/src/main/java/com/devfive/vespera/bridge/RequestShape.java` | ~79 | Java request body/idempotency classifier used by smart dispatch-mode selection |

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
  └── tokio (rt only — for dispatch_from_bytes Runtime param;
             + rt-multi-thread with feature `runtime` / `ffi`)

vespera_jni (JNI glue — thin layer)
  ├── vespera_inprocess (via workspace, features connect-info + runtime)
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
| `inprocess` | `vespera::inprocess` (= `vespera_inprocess`) | dispatch, register_app, envelopes; enables `vespera_inprocess/connect-info` (`remote_addr` wire field → `ConnectInfo<SocketAddr>`, which needs `axum/tokio`) |
| `jni` | `vespera::jni` (= `vespera_jni`) + implies `inprocess` | RUNTIME, jni_app!, JNI symbol |
| `ffi` | `vespera::ffi_app!` / `ffi_apps!`, `vespera::inprocess::ffi` + implies `inprocess` (`vespera_inprocess/ffi` → `runtime`) | stable C ABI exported from the cdylib (`vespera_dispatch` & co.); shares the host runtime with `jni`, so one cdylib can serve both (`rust-jni-demo` does) |
| `mimalloc` (default) | (with `jni`) mimalloc as the cdylib's `#[global_allocator]` | measured -15~19% on sync/direct dispatch vs Windows HeapAlloc; weak no-op without `jni` |
| `multipart` (default) | `vespera::multipart` (`TypedMultipart`, `FieldData`), `vespera::tempfile` | `#[derive(Multipart)]` / `schema_type!(..., multipart)` runtime + deps (tempfile, axum/axum-extra multer multipart, tokio fs/io-util); without it `vespera::Multipart` is a stand-in derive (`vespera_macro::MultipartFeatureDisabled`) whose `compile_error!` names the feature (trybuild `tests/ui/multipart_needs_feature.rs`, run under `--no-default-features`) |
| `validation` (default) | `vespera::Validated` | `#[derive(Schema)]` also emits `garde::Validate`; `Validated<T>` extractor + 422 envelope |
| `cron` | `vespera::cron` | `#[vespera::cron("...")]` jobs (UTC), started once per process by `vespera!` |
| `tracing` | (no API) forwards to `vespera_inprocess/tracing` | `vespera.dispatch` span per wire dispatch (`mode`/`method`/`path` sans query/`app`/`status`/`duration_ms`/`panicked`) + WARN/ERROR events from `error_wire` (400/413/431/503/504/5xx) and on panic unwinding out of a dispatch poll; cron diagnostics via `tracing` (target `vespera::cron`) instead of `eprintln!`. Off: no dep, `traced!` expands to the bare body. On in workspace test builds via `examples/testkit` (dev-dep) so tarpaulin covers it |
| `cron-tz` | (with `cron`) `tz = "Area/City"` support | chrono-tz IANA database (opt-in: ~7 MB generated source) |

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
                               "query"?, "headers"?, "app"?,
                               "remote_addr"?, "scheme"?,
                               "principal"? }
                  (response) { "v":1, "status", "headers",
                               "metadata", "validation_errors"? }
bytes 4+N..   : raw body bytes (UTF-8 text or binary —
                no encoding applied)
```

- No base64 — multipart uploads / PDFs / images travel as raw bytes.
- `"v":1` is the protocol version; mismatched versions get a `400` wire response.
- All failure modes (malformed wire, panic in Rust, no app registered) return a valid length-prefixed wire response, so the Java decoder never has to special-case errors.
- `validation_errors` is an optional array hoisted from 422 JSON bodies (`{"errors":[...]}`) — original body preserved verbatim alongside.
- Optional request fields `remote_addr` (`ip:port`) and `scheme` (`http`/`https`) become the `ConnectInfo<SocketAddr>` and `http::uri::Scheme` request extensions (unparseable values are ignored; the URI stays origin-form). The Java proxy fills them from the servlet request.
- Optional request field `principal` (`null` | `{"name", "authorities"?: [string], "attributes"?: {string:string}}`) is the **trusted side channel** for the authenticated caller: only the host writes it (Java: `PrincipalResolver` bean, default `ServletPrincipalResolver` = `getUserPrincipal()` + Spring Security authorities read reflectively; `vespera.bridge.forward-principal=false` disables), never from client headers. Dispatch inserts it as the `vespera_inprocess::Principal` request extension (also `RequestEnvelope::principal` on the text API); the `Principal` extractor rejects `401` when absent (`Option<Principal>` / `Extension<Principal>` also work). Parsed by `wire/header_read/principal.rs` (child module extending `Parser`; serde twin `bench_serde::de_opt_principal`), `authorities` / `attributes` capped at `MAX_PRINCIPAL_ENTRIES` = 1024 each (`400`).
- `metadata` stays a string→string map; a Rust panic answers `500` with `"panicked":"true"` in it (`vespera_inprocess::panic_error_wire`), which the bridge uses for WARN logs, `DispatchEvent.panicked` and the Micrometer `outcome=PANIC` tag.
- **Header values use a Latin-1 mapping** (`vespera_inprocess::header_latin1`, documented on the Java side in `WireHeaderStringSupport`): servlet containers expose header bytes as ISO-8859-1 strings, so each byte travels as the JSON char `U+0000–U+00FF`. Request: a value whose chars are all `≤ U+00FF` maps back to one byte per char; a value with any wider char keeps its UTF-8 bytes (non-servlet producers). Response: every value byte (including `obs-text ≥ 0x80`, formerly rendered as `""`) becomes one char, so `HttpServletResponse.addHeader` restores the raw bytes. ASCII is byte-identical (`wire_contract.rs`); the serde A/B twin applies the same mapping.

### JNI Dispatch Modes (seven dispatch symbols + `configure*0` hooks)

**Shutdown hook:** `shutdown0(long timeoutMillis)` (Java `VesperaBridge.shutdown(Duration)`, Rust `vespera_jni::shutdown_runtime`) shuts the shared Tokio runtime down once; afterwards every dispatch answers `503` (`RUNTIME_SHUT_DOWN_MSG`) and the runtime is never rebuilt. Spring calls it on context close only with `vespera.bridge.shutdown-on-close=true` and only for a library it loaded itself via `vespera.bridge.library-name`. The native library cannot be unloaded or loaded by a second class loader (redeploys need the shared container lib or a JVM restart).

| Symbol | Java native | Mode | Memory |
|---|---|---|---|
| `Java_...dispatchBytes` | `byte[] dispatchBytes(byte[])` | sync | full body |
| `Java_...AsyncDispatch_{dispatch0,cancel0}` | `void dispatchAsync(CompletableFuture<byte[]>, byte[])` (public Java wrapper over package-private `AsyncDispatch`) | async, cancellable | full body |
| `Java_...dispatchStreaming` | `byte[] dispatchStreaming(byte[], OutputStream)` | sync response-streaming | chunk-bounded response |
| `Java_...dispatchFullStreaming` | `byte[] dispatchFullStreaming(byte[], InputStream, OutputStream)` | sync bidirectional streaming | chunk-bounded both directions |
| `Java_...dispatchStreamingWithHeader` | `void dispatchStreamingWithHeader(byte[], Consumer<byte[]>, OutputStream)` | sync response-streaming, header callback before first body byte | chunk-bounded response |
| `Java_...dispatchFullStreamingWithHeader` | `void dispatchFullStreamingWithHeader(byte[], Consumer<byte[]>, InputStream, OutputStream)` | sync bidirectional streaming, header callback | chunk-bounded both directions |
| `Java_...dispatchDirect0` | `int dispatchDirect(ByteBuffer, int, ByteBuffer)` (public validated wrapper over the private native) | sync, direct buffers | full body, zero Java heap arrays |
| `Java_...DirectStash_{dispatchResumable0,fetch0,stream0,discard0}` | package-private `DirectStash` (used by `dispatchDirectPooled`) | sync, direct buffers + overflow stash | an overflow keeps the finished response natively: fetched into a grown buffer or streamed (header-first) — the handler runs once |

All share the same wire format, registered router, and panic-safe `catch_unwind` discipline. **`DecodedResponse` (vespera-bridge 0.2.0, BREAKING):** `body()` now returns a read-only `java.nio.ByteBuffer` (zero-copy view over the wire bytes); `bodyBytes()` materialises an owned `byte[]` copy on demand — callers that previously used `body()` as `byte[]` must switch to `bodyBytes()`. The direct-buffer path (`dispatchDirect` + pooled `dispatchDirectPooled`, per-thread 64 KiB→4 MiB buffers via `vespera.direct.maxBufferBytes`) removes the two JNI region copies of `dispatchBytes`; on response overflow the raw `dispatchDirect0` returns `-(requiredSize)` (a retry re-runs the handler), while the pooled path uses `DirectStash.dispatchResumable0`: the finished response is stashed natively and fetched into a grown buffer (no re-run, any method); beyond the pool cap or for `text/event-stream` it raises `StashedOverflowException` and the proxy streams it from the stash. Spring autoconfigured default since vespera-bridge 0.2.0: `SmartDispatchModeResolver` (`Accept: text/event-stream` / NDJSON → `STREAMING`, small/bodyless idempotent → `DIRECT` ~2.2µs, small non-idempotent → `SYNC` ~3.2µs, else streaming ~24µs — a body that fits one 256 KiB chunk now takes the inline fast path, ~2.9µs (64 B) / ~4.6µs (64 KiB)); on virtual threads SYNC never blocks natively across a wait (`ParkingDispatch`): since 0.6 it is poll-first — `AsyncDispatch.dispatchPollFirst0` (`crates/vespera_jni/src/jni_impl_park.rs`) polls the dispatch once on the caller (ready → inline wire, pinned-call cost), and only a pending future moves to a Tokio worker while the virtual thread parks on a `CompletableFuture`; `-Dvespera.virtualThread.pollFirst=false` = old always `dispatchAsync` + `join()` (~11x p50, ~1/5 throughput for a non-waiting handler; `libs/vespera-bridge/docs/virtual-thread-park.md`, harness `bench/VirtualThreadParkBench.java`). The header-first streaming variants flush the Java `OutputStream` (empty-chunk hint) whenever the body would wait, and coalesce already-ready small frames up to the chunk size before crossing JNI. `shutdown0` drains in-flight dispatches (every mode holds a `RuntimeLease`) before stopping the runtime; a dropped `dispatchAsync` task completes its future with `503`. Opt out with `vespera.bridge.dispatch-mode=bidirectional-streaming` to restore the pre-0.2.0 default (`BidirectionalStreamingDispatchModeResolver`: provably bodyless requests — CL:0, or GET/HEAD/OPTIONS without CL/TE — downgrade to response-only `STREAMING` ~3x, 24.1→7.7µs; everything else streams both ways). `dispatchAsync` spawns the dispatch on Rust's shared Tokio runtime via `tokio::spawn` (panic → `JoinError` → `error_wire(500)`) and completes the `CompletableFuture` from a daemon-attached Tokio worker thread (`with_cached_daemon_env` from `daemon_env.rs`, called from the `dispatchAsync` completion closure in `jni_impl.rs`: `GetEnv` first, `AttachCurrentThreadAsDaemon` only for unattached threads, per-completion local frame + unconditional pending-exception cleanup). In-flight async dispatches are capped (`vespera.async.maxInFlight`, default 4096, 0 = unlimited) — over the cap the future completes immediately with a `503` wire. **Cancel-on-disconnect:** `AsyncDispatch.dispatch0` registers the spawned task with `vespera_inprocess::cancellable` (sharded token registry, registered before the spawn, deregistered when the future drops) and returns the token; `AsyncDispatch.linkCancellation` calls `cancel0(token)` when the Java future completes **exceptionally** (`cancel`, `orTimeout`, external `completeExceptionally` — the native side only ever completes normally), dropping the handler at its next poll and releasing the in-flight permit + `RuntimeLease` with the task; the proxy's ASYNC flow cancels on Spring async timeout / error / completion. Streaming modes stop on the first failed servlet write/flush (`make_push_closure` → `ControlFlow::Break` → `StreamOutcome::SinkStopped`, body dropped before return). SYNC/DIRECT are not cancellable (disconnect only visible after the handler finished; `dispatch.timeoutMillis` is the bound). `dispatchBytes` / `dispatchDirect0` (SYNC/DIRECT) `block_on` the **same shared multi-thread runtime** from the Java caller thread, so pooled I/O resources (DB pools, HTTP clients) and detached `tokio::spawn` tasks behave identically in every dispatch mode (no per-thread runtime). `dispatchStreaming` drains the response body chunk-by-chunk via `http_body::Body::frame()` and writes each chunk to the Java `OutputStream`; it hoists 422 `validation_errors` like the buffered paths, and a body/sink failure after chunks were written surfaces as an `IOException` (Rust: `dispatch_streaming_with_outcome_async`) instead of a synthetic 500 after a partial body (same for `dispatchFullStreaming`). `dispatchFullStreaming` adds request-side streaming: a `tokio::task::spawn_blocking` thread pulls chunks (default 256 KiB) from `InputStream.read(byte[])` and feeds them into axum via an `mpsc::channel`-backed `http_body::Body`, giving natural backpressure (bounded channel, default 16 slots) so 1 GiB uploads run in `O(chunk_size)` RAM (optional cap: `vespera.limits.maxStreamingRequestBytes`, default unlimited → `413`). Small-body fast path (`streaming/inline_pull.rs`): with no dispatch timeout, the first two pulls the handler makes **before returning its response** run inline on the `block_on` (Java caller) thread, so a body that ends within them (one chunk + EOF) never starts the producer, channel or producer slot (Rust `bidirectional_small` bench, 64 B echo: ~10.5 → ~0.95 µs; JNI `dispatchFullStreaming` echo: 64 B ~18.9 → ~2.9 µs, 64 KiB ~24 → ~4.6 µs); pulls made while the response streams, inside a spawned task, or under a deadline always use the producer. When the handler responds early, the response waits at most ~2 s for the upload reader before detaching it.

**Streaming tuning (process-fixed after first dispatch):** chunk size via system property `vespera.streaming.chunkBytes` / env `VESPERA_STREAMING_CHUNK_BYTES` (default 256 KiB, clamped 4 KiB–8 MiB); channel capacity via `vespera.streaming.channelCapacity` / `VESPERA_STREAMING_CHANNEL_CAPACITY` (default 16, clamped 1–1024). Java API: `VesperaBridge.configureStreaming(chunkBytes, channelCapacity)` — pending-config pattern (call before `init()`; values stored pending and applied right after native load, before any dispatch; programmatic > sysprops > env > defaults). Rust-side setters: `vespera_inprocess::set_streaming_chunk_bytes` / `set_streaming_channel_capacity` (precedence: setter > env > default). The shared Tokio runtime's worker count is tunable the same way: `vespera.runtime.workerThreads` / `VESPERA_RUNTIME_WORKERS` (default: logical CPUs, clamped 1–1024) — cap it when JVM thread pools compete for the same cores; its blocking pool via `vespera.runtime.maxBlockingThreads` / `VESPERA_RUNTIME_MAX_BLOCKING_THREADS` (default 512, 1–8192).

**Limits & deadline (process-fixed after first dispatch; setter > sysprop > env > default):**

| Sysprop / env | Default | Effect |
|---|---|---|
| `vespera.limits.maxRequestBytes` / `VESPERA_MAX_REQUEST_BYTES` | 64 MiB (0 = unlimited) | buffered request body cap → `413` |
| `vespera.limits.maxStreamingRequestBytes` / `VESPERA_MAX_STREAMING_REQUEST_BYTES` | 0 = unlimited | streamed request body cap → `413` |
| `vespera.limits.maxBufferedResponseBytes` / `VESPERA_MAX_BUFFERED_RESPONSE_BYTES` | 256 MiB | buffered response cap → `500` (grown with `try_reserve`; 422 hoist reads ≤ 64 KiB+1) |
| `vespera.dispatch.timeoutMillis` / `VESPERA_DISPATCH_TIMEOUT_MS` | 0 = disabled | deadline → `504`: buffered/direct bound the whole dispatch; streaming modes bound only time-to-first-header (no total deadline once the header is committed) |
| `vespera.async.maxInFlight` / `VESPERA_ASYNC_MAX_IN_FLIGHT` | 4096 (0 = unlimited) | `dispatchAsync` concurrency → `503` |
| `vespera.streaming.maxRequestProducers` / `VESPERA_MAX_STREAMING_REQUEST_PRODUCERS` | 256 (0 = unlimited) | concurrent bidirectional upload producers on the blocking pool; extra uploads wait for a slot |
| (fixed) `MAX_REQUEST_HEADERS` | 1024 | more request headers → `431` (hand-rolled parser + `build_axum_request`) |

Java: `VesperaBridge.configureLimits(...)`, `configureDispatchTimeout(ms)`, `configureAsyncMaxInFlight(n)`, `configureRuntime(workers, maxBlocking)` — all return `boolean` (false + `System.Logger` warning when applied too late; `configureStreaming` stays `void` for 0.5.x binary compatibility); unset sentinels (`<= 0` runtime, `< 0` others) are never stored, so they never shadow sysprops; pending-config state lives in `VesperaNativeConfig`. Bridge JAR and cdylib must come from the same release (`configure*0` return `jboolean`). Spring: the same knobs under `vespera.bridge.native.*` (`worker-threads`, `max-blocking-threads`, `max-request-bytes`, `max-streaming-request-bytes`, `max-buffered-response-bytes`, `dispatch-timeout`, `async-max-in-flight`), forwarded by the `vesperaBridgeNativeSettings` bean before the proxy controller is created. `_default`-app dispatch resolves through a lock-free `OnceLock<Router>` fast path; named apps are also lock-free — a single `ArcSwap<HashMap<String, Router>>` load plus a hash lookup. The response wire header serializes straight from `http::HeaderMap` (zero per-header allocation) and request wire headers deserialize borrowing from the input buffer (`Cow`); both the response serialize and the request parse use **hand-rolled** writers/parsers (`wire/header_write.rs` / `wire/header_read.rs`) byte-identical to the prior `serde_json` path — the serde twins are retained private as `*_serde` for the same-run criterion A/B in `benches/dispatch.rs` (group `wire_header_serde`). The wire byte layout is locked by `crates/vespera_inprocess/tests/wire_contract.rs` and the hand-vs-serde round-trip property test in `wire.rs`.

### Rust Public API (vespera_inprocess)

| Function | Sig | Use |
|---|---|---|
| `register_app(F)` | sync | Register the default app (first-wins, BC) |
| `register_app_named(&str, F)` | sync | Register a named app for multi-app routing |
| `dispatch_from_bytes(Vec<u8>, &Runtime) -> Vec<u8>` | sync | FFI entry, blocks on runtime |
| `dispatch_from_bytes_async(Vec<u8>) -> Vec<u8>` (async) | async | inside an existing runtime |
| `dispatch_streaming_async<F>(Vec<u8>, F) -> Vec<u8>` (async) | response streaming async (422 hoisted) | `F: FnMut(&[u8])` body chunks |
| `dispatch_streaming_with_outcome_async<F>(Vec<u8>, F)` (async) | response streaming + outcome | distinguishes a mid-body abort/timeout from a completed response |
| `dispatch_streaming_with_header_async<H,F>(Vec<u8>, H, F)` (async) | response streaming, header callback first | `H: FnMut(&[u8])` fires before first body chunk |
| `dispatch_bidirectional_streaming<P,F>(Vec<u8>, P, F) -> Vec<u8>` (async) | bidirectional streaming | `P: FnMut() -> Option<Vec<u8>> + Send + 'static`, `F: FnMut(&[u8])` |
| `dispatch_bidirectional_streaming_with_header<P,F,H>(Vec<u8>, P, F, H)` (async) | bidirectional streaming, header callback | header before first body chunk |
| `dispatch_into(Vec<u8>, &mut [u8], &Runtime) -> DirectWriteResult` | sync | direct-write FFI entry — wire response streamed straight into the caller's buffer (no response `Vec`); `Complete(n)` / `Overflow(exact_required)`; 422 materialised internally to keep `validation_errors` hoisting |
| `dispatch_into_async(Vec<u8>, &mut [u8]) -> DirectWriteResult` (async) | async | same, inside an existing runtime |
| `error_wire(u16, &str) -> Vec<u8>` | sync | wire-format error builder |
| `cancellable(F) -> (u64, impl Future<Output = Option<F::Output>>)` / `cancel(u64) -> bool` | sync | host-driven cancellation of a spawned dispatch (JNI `dispatchAsync` cancel) |
| `dispatch_typed(Router, &RequestEnvelope) -> ResponseEnvelope` | async | direct axum API (BC) |
| `runtime::{runtime, block_on_shared_runtime, shutdown_runtime, on_shared_runtime, ...}` | sync | feature `runtime`: the shared host runtime (JNI + C ABI) |

### C ABI (`vespera_inprocess::ffi`, feature `ffi`)

Header `crates/vespera_inprocess/include/vespera.h` (hand-written; `tests/ffi_header.rs` compares every prototype / `#define` with the Rust items and coerces each symbol to its C function-pointer type). `ffi_app!` / `ffi_apps!` (re-exported as `vespera::ffi_app!` / `ffi_apps!`, names checked at compile time like `jni_apps!`) emit `vespera_init` in the user's cdylib; the library exports the rest.

| Symbol | Contract |
|---|---|
| `int vespera_init(void)` | marks the host loaded (cron), registers the apps (first-wins → idempotent), builds the runtime; `VESPERA_ERR_REGISTRATION` / `_PANIC` / `_RUNTIME` |
| `int vespera_dispatch(req, len, uint8_t **out, size_t *out_len)` | always `VESPERA_OK` + wire response (`400` NULL/malformed, `404` app, `413`, `500` panic, `503` shut down) unless `out`/`out_len` NULL → `VESPERA_ERR_INVALID_ARGUMENT` |
| `int vespera_dispatch_into(req, len, out, cap, size_t *written)` | `VESPERA_OK` (`*written` = n) / `VESPERA_OVERFLOW` (`*written` = required; retry re-runs the handler; `NULL`/`0` = size probe); overlapping buffers rejected |
| `int vespera_dispatch_async(req, len, cb, user_data)` | `cb(user_data, resp, resp_len)` exactly once (inline when rejected early, else on a worker; `503` when dropped by shutdown); `cb` owns `resp` |
| `void vespera_free(ptr, len)` | frees a response (`Box<[u8]>`); NULL no-op |
| `int vespera_shutdown(uint64_t timeout_ms)` | `shutdown_runtime`; `VESPERA_ERR_RUNTIME` when already shut down |

Sync symbols must not be called from a runtime worker (the `block_on` panic degrades to a `500` panic wire).

### Multi-app routing

**Use case**: multi-app is primarily a feature for **external-dispatcher scenarios** — JNI (Java host picks app per request via header), WebAssembly bridge, C FFI, or any in-process embedding where the host distinguishes between multiple independent vespera API surfaces.  For Rust **standalone** servers (`axum::serve(...)`), the native axum patterns (`Router::merge()`, `Router::nest()`) are more idiomatic for modularization — `register_app_named` adds no value when the same binary owns both the router registration and the HTTP entry point.

The wire header carries an optional `"app": "<name>"` field (default
omitted → `"_default"` app).  Dispatch looks the name up in
`APP_ROUTERS: ArcSwap<HashMap<String, Router>>` — resolution is
lock-free (one atomic load + hash lookup) — and returns:

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
`VesperaProxyController` + three strategy beans, all replaceable via
`@ConditionalOnMissingBean`:

- `AppNameResolver` (default: `HeaderAppNameResolver("X-Vespera-App", vespera.bridge.exposed-apps)`) — picks app per request. The header is client-controlled: `vespera.bridge.exposed-apps` allowlists selectable apps (others → proxy `404`, no dispatch; unset → one-time WARN on first non-default selection); the selection header is never forwarded to Rust
- `DispatchModeResolver` (default since vespera-bridge 0.2.0: `SmartDispatchModeResolver` — small/bodyless idempotent → `DIRECT` ~2.2µs, small non-idempotent → `SYNC` ~3.2µs, else `BIDIRECTIONAL_STREAMING` ~24µs, or ~3–5µs when the body fits one chunk (inline fast path); `vespera.bridge.dispatch-mode=bidirectional-streaming` restores pre-0.2.0 `BidirectionalStreamingDispatchModeResolver` — bodyless requests take response-only `STREAMING`, everything else bidirectional) — picks `DispatchMode`
- `PrincipalResolver` (default: `ServletPrincipalResolver`, or `PrincipalResolver.none()` with `vespera.bridge.forward-principal=false`) — the trusted principal forwarded in the `principal` wire field

Property `vespera.bridge.controller-enabled=false` disables the whole controller for BYO scenarios; `vespera.bridge.include-patterns` (default `/**`) / `exclude-patterns` (PathPattern) narrow the proxy's mapping instead — `VesperaProxyPathMappings` re-registers the `/**` `RequestMappingInfo` with the includes + an exclusion `RequestCondition` (no-op with the defaults), so excluded paths fall through to other handler mappings / static resources / `404`. The JAR ships `spring-configuration-metadata.json` (configuration processor + `additional-spring-configuration-metadata.json`; `ConfigurationMetadataTest` asserts every bound and `@ConditionalOnProperty` key is documented).  See [`libs/vespera-bridge/README.md`](libs/vespera-bridge/README.md#customization) for the customization recipes.

**WebFlux:** `VesperaBridgeReactiveAutoConfiguration` is the `REACTIVE` counterpart (the servlet one stays `SERVLET`-only; WebFlux/Reactor are `compileOnly` and `verifyPublishedPom` — finalizer of `test`, part of `check` — fails if the POM / module metadata declares any dependency). Strategy beans: `ReactiveAppNameResolver` (default `ReactiveHeaderAppNameResolver`, sharing `AppHeaderSelection` with the servlet resolver) and `ReactivePrincipalResolver` (default `ExchangePrincipalResolver`; reflective authorities in `SecurityAuthorities`). `VesperaReactiveHandlerMapping` reuses `ProxyPathPatterns.matches(PathContainer)`; `VesperaReactivePreflightFilter` routes CORS preflights to Rust (WebFlux never invokes a handler for them). `dispatch-mode`, `direct-retry-on-overflow`, `async-pool-size`, `clear-threadlocals-after-request` do not apply. Tests use `WebTestClient` / reactor-netty with a fake `Transport` and avoid the WebTestClient assertion DSL (binary-incompatible between Spring 6.2 and 7 under `testBoot3`) via `ReactiveTestSupport.call`. See the README `WebFlux` section.

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
| Cargo (`Cargo.toml`) | cargo | `crates/*`, `examples/*`, `benches/*` — `default-members` = `crates/*` + `examples/*` (the compile-time benches build only with `--workspace` / `-p`) | OpenAPI engine, proc-macros, JNI bridge |
| Bun (`package.json`) | bun | `apps/*` (currently only `apps/landing`), `e2e/*` (currently only `e2e/devup-api`) | Marketing/docs site (Next.js); generated-client E2E checks |

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
bun run dev                           # `dev` in every workspace that has one (--if-present)
bun run build                         # `bun run -F ./apps/landing build`
cd apps/landing && bun dev            # Single-app dev (preferred per devfive-frontend)

# --- Tests (Bun side) ---
bun test                              # Front tests from the repo root
bun run test                          # Same, plus tarpaulin/doctests (posttest hook)
cd apps/landing && bun test           # Same tests from the app directory

# --- Release tooling ---
bun run changepacks                   # @changepacks/cli version bumps

# --- Perf regression triage ---
# Bisect ONE dependency across crates.io releases (exit 0 = good, non-zero = bad;
# Cargo.lock is restored afterwards). See the header of each script.
scripts/bisect-dep.sh <crate> <good> <bad> <probe command...>
./scripts/bisect-dep.ps1 -Crate <crate> -Good <v> -Bad <v> -Command '<probe>'
```

> **`prelint` gotcha:** any Rust warning fails the JS lint. Run `bun run
> lint:fix` to auto-resolve both sides.

### Frontend (`apps/landing`)

Next.js 16 App Router + React 19 + `@devup-ui/react` (build-time CSS-in-JS).
Theme tokens live in `apps/landing/devup.json` and use `$token` syntax in JSX
props only.

- `apps/landing/src/app/` holds **only** route files (`layout.tsx`, `page.tsx`,
  and route-local `_components/`) — shared components live in `src/components/`
  (per devfive-frontend conventions).
- Styling uses devup-ui shorthand props (`bg`, `p`, `w`, `_hover`,
  `[mobile,null,pc]` responsive arrays). Never `style={{...}}` or Tailwind.

### Where Tests Live

| Concern | Location |
|---|---|
| Macro integration tests | `crates/vespera_macro/tests/` (+ `insta` snapshots) |
| Macro compile-fail diagnostics | `crates/vespera_macro/tests/ui/` + `crates/vespera/tests/ui/` (trybuild, `trybuild_diagnostics.rs`; regenerate `.stderr` with `TRYBUILD=overwrite`, then review) |
| Wire parser fuzzing | `fuzz/` (cargo-fuzz, own workspace: `wire_dispatch`, differential `wire_header_parse` vs the serde twin); Java: `WireHeaderReaderFuzzTest` (seeded) |
| GraalVM metadata ↔ JNI lookups | `libs/vespera-bridge/src/test/java/com/devfive/vespera/bridge/NativeImageMetadataTest.java` (reads `crates/vespera_jni/src`: every `CachedMethod::resolve` / `call_method` / `throw_new` / `jni_str!` class must be in `jni-config.json`; any other JNI lookup API call fails until an extraction rule covers it; pins the audited `jni` crate version — on a bump re-trace with `-agentlib:native-image-agent` over the demo-app E2E suite) |
| Validated/422 contract | `crates/vespera/tests/validated_extractor.rs`, `crates/vespera/tests/jni_validation.rs` | Envelope built via `#[derive(Serialize)]` structs (not `serde_json::json!`); exact bytes locked by `insta::assert_snapshot!` in `validated_extractor.rs` |
| Core unit tests | `crates/vespera_core/src/**` inline `#[cfg(test)]` |
| JNI end-to-end | `examples/rust-jni-demo` (Rust + Java + Gradle); WebFlux: `java/demo-reactive-app` (`ReactiveProxyE2ETest`, real cdylib, no servlet API) |
| Front tests | `apps/landing/src/__tests__/` (`bun test` + `bun-test-env-dom`) |
| OpenAPI → TS client E2E | `e2e/devup-api/` (`bun test`): `examples/axum-example/openapi.json` → `@devup-api/generator` (pinned) → `df/api.d.ts` (gitignored) → `tsc --noEmit` over `src/calls.ts` (positive calls + `@ts-expect-error` precision probes), plus `@devup-api/fetch` requests against a stubbed `fetch`. Known generator gaps are listed at the top of `src/calls.ts` |

> **Front tests need the bun preloads.** `bunfig.toml` preloads
> `@devup-ui/bun-plugin` (without it every `@devup-ui/react` component throws
> `Cannot run on the runtime`) and `bun-test-env-dom` (happy-dom + readable HTML
> snapshots). Bun reads `bunfig.toml` only from the CWD, so the file is mirrored
> at the repo root and in `apps/landing/` — `bun test` works from either. The
> devup plugin also resolves `devup.json` / writes `df/` relative to the CWD;
> the root `devup.json` is a one-line `extends` of `apps/landing/devup.json` so
> both runs extract identical classes (the generated `df/` self-ignores).
> Components pulling context from `app/layout.tsx` (e.g. `useHeader`) must be
> wrapped in the same providers inside the test.

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
- **Java build**: Gradle (Kotlin DSL), published to Maven Central (`kr.devfive:vespera-bridge`, `kr.devfive:vespera-bridge-gradle-plugin`) via changepacks → `./gradlew publishToMavenCentral` (vanniktech maven-publish + GPG in-memory signing); both apply `org.cyclonedx.bom` (SBOM, see CI notes)
- **One version for every package**: the five `vespera*` crates, `vespera-bridge` and the Gradle plugin always share a version. List **all seven** packages in every `.changepacks/changepack_log_*.json` with the **same** level (0.x: breaking = `Minor`); `bun scripts/check-version-alignment.ts` (CI lint job) fails otherwise.

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

# Java side (the suites the jni-e2e CI job runs)
cd libs/vespera-bridge && ./gradlew test          # bridge unit suite
cd libs/vespera-bridge-gradle-plugin && ./gradlew test  # Gradle plugin suite
cd examples/rust-jni-demo/java && ./gradlew :demo-app:test  # JNI E2E

# CI: jni-e2e job (JDK 17 on ubuntu/windows/macos + ubuntu-24.04-arm +
# macos-15-intel; JDK 21/25 on ubuntu) runs the vespera-bridge
# unit suite AND the demo-app E2E tests (incl. StreamingClosureStressTest);
# the JDK 17 legs also run the vespera-bridge-gradle-plugin test suite.
# It is also the coverage for `crates/vespera_jni`, which tarpaulin excludes
# (`#![cfg(not(tarpaulin_include))]`) because its symbols need a live JVM.
# See .github/workflows/CI.yml

# Other CI gates (.github/workflows/CI.yml) — all required before the main-only
# changepacks release job, the only job that receives publish secrets:
#   lint     cargo fmt --all --check + both clippy feature sets (= `prelint`),
#            bun install --frozen-lockfile, oxlint, bun test
#   msrv     cargo check --all-features on Rust 1.88 (`rust-version` in the root
#            Cargo.toml) for the 5 published crates — examples need newer Rust
#   deny     cargo deny check (deny.toml: advisories, licenses, bans, sources)
#   sbom     scripts/sbom.sh: CycloneDX SBOMs of all 7 packages (5 crates via a
#            hash-pinned cargo-cyclonedx; both Java libs via org.cyclonedx.bom
#            `cyclonedxDirectBom`), uploaded as the `sbom` workflow artifact
#   test-os  cargo test --workspace on windows-latest + macos-latest
#   semver   (PRs only, not a release gate) cargo-semver-checks per published
#            crate; release type is read from .changepacks/changepack_log_*.json,
#            so an intentional break must be declared as a Minor (0.x) / Major bump
#   miri     cargo miri nextest run (pinned nightly) over every vespera_inprocess
#            unit test + the JNI-free unsafe helpers / gates of vespera_jni;
#            `#[cfg_attr(miri, ignore = "...")]` only for child-process or
#            pathologically slow tests (`.config/nextest.toml` default-miri)
#   loom     RUSTFLAGS="--cfg vespera_loom" cargo test --release --lib loom_
#            (LOOM_MAX_PREEMPTIONS=3), twice: `-p vespera_inprocess --features
#            runtime` (runtime lease gate vs shutdown drain,
#            `vespera_inprocess/src/runtime/gate.rs`) and `-p vespera_jni`
#            (dispatchAsync cap, `jni_impl_gates.rs`). Custom cfg, NOT
#            `cfg(loom)` — that flips tokio into its own loom build
#   bench           (bench.yml, PRs touching crates/**) criterion gate on the
#                   in-process hot path: merge base vs head, 5 interleaved
#                   paired rounds, FAIL iff median > +30% AND every round > +20%
#                   (scripts/bench-interleaved.sh)
#   jni-bench-gate  (jni-bench-gate.yml, PRs touching vespera_inprocess /
#                   vespera_jni / libs/vespera-bridge) the same rule over real
#                   JNI dispatch: rust-jni-demo cdylib (pinned alignment
#                   RUSTFLAGS) + bridge JAR per side, one fresh JVM per side per
#                   round running libs/vespera-bridge/bench/JniDispatchBench.java
#                   (sync / direct / streaming GET, 64 KiB full-streaming POST,
#                   async GET); logic in scripts/jni-bench-interleaved.sh +
#                   scripts/jni-bench-compare.ts (unit-tested by `bun test`).
#                   Local: CARGO_JOBS=2 scripts/jni-bench-interleaved.sh origin/main
#                   jni-bench.yml stays nightly + informational (*BenchTest suite).
#   fuzz-build (not a release gate) `cargo fuzz build` of fuzz/ on pinned nightly
#   native-image (native-image.yml, workflow_dispatch only, never a gate)
#            GraalVM CE 25 `-PnativeImage :demo-app:nativeCompile` (bridge +
#            plugin as --include-build, no mavenLocal) + scripts/native-image-smoke.sh
#            (direct / overflow / sync / async-on-virtual-threads / SSE /
#            bidirectional / 422 / panic). Local Windows needs MSVC (vcvars64).
# Fuzz workflow (.github/workflows/fuzz.yml, weekly + workflow_dispatch, never a
# gate): runs every fuzz/ target for a bounded time (default 600 s each) and
# uploads crash reproducers. Dependabot (.github/dependabot.yml): weekly grouped
# minor/patch PRs for cargo, the three Gradle builds, bun and github-actions
# (dtolnay/rust-toolchain + changepacks/action are branch-pinned → ignored).
# Third-party actions are pinned to commit SHAs; workflows default to
# `permissions: contents: read`. After changepacks publishes (even partially),
# the release job runs scripts/release-artifacts.sh: it collects the published
# .crate files (sha256 checked against crates.io) + JARs and regenerates the
# SBOMs from the released commit in a temp worktree (a package that fails is
# skipped, the rest still collected), then uploads them as the
# `release-attestation-subjects` artifact. Only the separate `attest` job
# (needs: changepacks; no checkout, only GitHub's attest actions) has
# `id-token: write` + `attestations: write` — the release job runs the
# unpinned changepacks CLI + build code and must not mint OIDC tokens. It runs
# actions/attest-build-provenance (all files) + one actions/attest SBOM
# attestation per package and attaches the SBOMs to the GitHub releases (best
# effort); "Re-run failed jobs" re-attests without publishing again.
# vespera-bridge also publishes its SBOM to Maven Central (classifier
# `cyclonedx`); the Gradle plugin's is NOT on its publication (the Plugin Portal
# rejects JSON artifacts). User-facing verification: SECURITY.md.
cargo deny check                                   # local equivalent of `deny`
cargo +nightly-2026-08-29 miri nextest run -p vespera_inprocess --features runtime --lib \
  --target x86_64-unknown-linux-gnu                # local `miri` (Linux target: tokio's
                                                   # Windows IOCP driver is unsupported)
RUSTFLAGS="--cfg vespera_loom" CARGO_TARGET_DIR=target/loom \
  cargo test --release -p vespera_inprocess --features runtime --lib loom_  # local `loom`
RUSTFLAGS="--cfg vespera_loom" CARGO_TARGET_DIR=target/loom \
  cargo test --release -p vespera_jni --lib loom_  # (both crates)
cargo +1.88 check --all-features -p vespera -p vespera_core -p vespera_macro \
  -p vespera_inprocess -p vespera_jni              # local equivalent of `msrv`

# Rust BRANCH coverage (observational — the rust-branch-coverage CI job, which
# never gates). tarpaulin cannot measure it at all: `--branch` is literally
# "NOT IMPLEMENTED". cargo-llvm-cov can, on nightly. Read it as a trend only —
# rust-lang/rust#124118 does not instrument `?`, `.await`, match arms /
# or-patterns, or ANY macro-expanded branch, so vespera_macro's generated code
# is dropped from the denominator rather than reported as uncovered.
# `--branch` goes on the instrumented run; `report` rejects it.
cargo +nightly llvm-cov --branch --workspace --no-fail-fast --no-report
cargo llvm-cov report --summary-only \
  --ignore-filename-regex '(benches|examples|vespera_jni)'
```

## NOTES

- Macro performs **filesystem I/O at compile time** - may affect IDE performance
- OpenAPI files are **regenerated on every build** when `openapi = "..."` specified
- `CARGO_MANIFEST_DIR` env var used to locate `src/routes/` folder
- Generic types in schemas require `#[derive(Schema)]` on all type params
- JNI native library can be bundled inside the fat JAR for single-file deployment
- `VesperaBridge.init()` auto-extracts bundled native lib to temp, falls back to system path
- JNI dispatch perf benchmarks: `libs/vespera-bridge/docs/jni-before-after-2026-06-11.md` (note: root `/docs` is gitignored); PR regression gate: `jni-bench-gate.yml` (harness `libs/vespera-bridge/bench/JniDispatchBench.java`)
- `vespera_macro` file_cache: per-macro-invocation epoch caching of `fs::metadata` (`bump_epoch` called at every file-cache-reaching entry point — `vespera!`, `schema_type!`, `schema!`, `export_app!`, `#[derive(Schema)]`); `collector.rs` clone-optimized
