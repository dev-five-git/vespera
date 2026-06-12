# JNI BEFORE ↔ AFTER benchmark report (2026-06-11)

## Headline

The v0.2.0 JNI break is justified by the hot-path wins it unlocks: the new `direct_pooled` ByteBuffer path completes the tiny `/health` round-trip in **2,349 ns/op**, **1.55× faster than the 0.1.1-era sync baseline** (3,643 ns/op), and the existing sync byte-array path is still **20% faster** after the series. The largest measured gains are in binary streaming throughput: AFTER is **2.14× to 3.26× faster** across 16 KiB → 256 KiB chunks, peaking at **14,458 MiB/s** for 256 KiB chunks versus **4,440 MiB/s** BEFORE. Response decoding now exposes the zero-copy API that did not exist BEFORE; that API gap is the core reason the breaking change is worth taking.

Small-request streaming and async latency did **not** improve in this run: response-only streaming, bidirectional streaming, and async-completable-future medians regressed versus the backported 0.1.1 harness. The async row is called out below as gate input for the follow-up attach/JMethodID optimization decision.

## Latency table

Protocol: 3 JVM invocations per side; run 1 discarded as cold; table value is the median of runs 2–3 (for two retained values, arithmetic midpoint). Lower is better.

| mode | BEFORE ns/op | AFTER ns/op | delta | speedup |
|---|---:|---:|---:|---:|
| `sync_dispatch_bytes` | 3,643 | 2,930 | -713 ns (-19.6%) | 1.24× faster |
| `direct_pooled` | N/A[^direct-na] | 2,349 | N/A | N/A |
| `response_streaming_only` | 3,735 | 6,922 | +3,187 ns (+85.3%) | 0.54× |
| `bidirectional_streaming` | 11,752 | 20,988 | +9,236 ns (+78.6%) | 0.56× |
| `async_completable_future` | 22,071 | 23,869 | +1,798 ns (+8.1%) | 0.92× |

[^direct-na]: `dispatchDirectPooled` / direct `ByteBuffer` dispatch did not exist in the 0.1.1 bridge, so the BEFORE harness drops this mode. Compared to the old BEFORE `sync_dispatch_bytes` baseline, AFTER `direct_pooled` is **1.55× faster**.

## Throughput table

Protocol: 64 MiB payload, 3 warmup iterations + 10 measured iterations per JVM; 3 JVM invocations per chunk size per side; run 1 discarded as cold; table value is the median of runs 2–3. Higher is better.

| chunkBytes | BEFORE MiB/s | AFTER MiB/s | delta |
|---:|---:|---:|---:|
| 16,384 | 4,859.8 | 10,407.9 | +5,548.2 MiB/s (+114.2%, 2.14×) |
| 65,536 | 4,711.3 | 11,587.0 | +6,875.7 MiB/s (+146.0%, 2.46×) |
| 262,144 | 4,439.9 | 14,458.3 | +10,018.5 MiB/s (+225.6%, 3.26×) |

## Raw measured values

Logs are retained in `%TEMP%` as `bench-before-*.log` and `bench-after-*.log`.

### Small request latency (`ns/op`)

| side | run | `sync_dispatch_bytes` | `direct_pooled` | `response_streaming_only` | `bidirectional_streaming` | `async_completable_future` |
|---|---:|---:|---:|---:|---:|---:|
| BEFORE | 1 (discarded) | 3,201 | N/A | 3,531 | 12,101 | 21,381 |
| BEFORE | 2 | 3,867 | N/A | 3,932 | 13,188 | 21,664 |
| BEFORE | 3 | 3,419 | N/A | 3,538 | 10,315 | 22,478 |
| AFTER | 1 (discarded) | 3,026 | 2,223 | 6,485 | 20,150 | 25,163 |
| AFTER | 2 | 2,872 | 2,221 | 6,475 | 18,947 | 25,444 |
| AFTER | 3 | 2,987 | 2,476 | 7,368 | 23,029 | 22,294 |

### Streaming throughput (`MiB/s`, mean ± stddev printed by the test)

| side | chunkBytes | run | throughput | stddev |
|---|---:|---:|---:|---:|
| BEFORE | 16,384 | 1 (discarded) | 5,039.0 | 754.0 |
| BEFORE | 16,384 | 2 | 4,732.4 | 565.3 |
| BEFORE | 16,384 | 3 | 4,987.1 | 702.3 |
| BEFORE | 65,536 | 1 (discarded) | 5,007.3 | 660.6 |
| BEFORE | 65,536 | 2 | 4,627.3 | 577.8 |
| BEFORE | 65,536 | 3 | 4,795.3 | 738.8 |
| BEFORE | 262,144 | 1 (discarded) | 4,966.2 | 686.1 |
| BEFORE | 262,144 | 2 | 4,485.1 | 618.3 |
| BEFORE | 262,144 | 3 | 4,394.6 | 540.1 |
| AFTER | 16,384 | 1 (discarded) | 10,446.8 | 772.1 |
| AFTER | 16,384 | 2 | 10,377.0 | 1,270.2 |
| AFTER | 16,384 | 3 | 10,438.8 | 991.3 |
| AFTER | 65,536 | 1 (discarded) | 13,017.3 | 1,898.4 |
| AFTER | 65,536 | 2 | 12,882.9 | 1,952.3 |
| AFTER | 65,536 | 3 | 10,291.1 | 1,868.3 |
| AFTER | 262,144 | 1 (discarded) | 13,140.2 | 2,093.0 |
| AFTER | 262,144 | 2 | 13,907.1 | 1,462.6 |
| AFTER | 262,144 | 3 | 15,009.5 | 1,011.7 |

## Gate input: `async_completable_future`

`async_completable_future` was explicitly measured on both sides with the same backported harness. BEFORE retained runs were **21,664** and **22,478 ns/op** (median **22,071 ns/op**). AFTER retained runs were **25,444** and **22,294 ns/op** (median **23,869 ns/op**). That is an **8.1% latency regression** in this protocol, so attach/JMethodID async follow-up should be decided from this row rather than inferred from Rust-side criterion or from sync/direct results.

## Methodology

- BEFORE base commit: `6242533483056b20bb363c34917133a395044aa8` (`6242533`).
- BEFORE throwaway worktree head for the measurement: `01592f4cca9649fdfe9a0d68503a38284a37ad66` on branch `before-bench-harness`.
- AFTER commit: `015a444b2f1dd50c8ab0c4a7c2729aac2b1aa58e` from the main working tree.
- Java: `openjdk version "21.0.8" 2025-07-15 LTS`, `OpenJDK Runtime Environment Zulu21.44+17-CA (build 21.0.8+9-LTS)`, `OpenJDK 64-Bit Server VM Zulu21.44+17-CA (build 21.0.8+9-LTS, mixed mode, sharing)`.
- Cargo: `cargo 1.96.0 (30a34c682 2026-05-25)`.
- OS/CPU: Microsoft Windows 11 Pro 10.0.26200; AMD Ryzen 9 9950X 16-Core Processor; 16 cores / 32 logical processors.
- Small-request benchmark: `SmallRequestLatencyBenchTest`, 20,000 warmup iterations + 100,000 measured iterations, `-Dvespera.bench=true`.
- Streaming benchmark: `StreamingThroughputBenchTest`, 64 MiB payload, 3 warmup iterations + 10 measured iterations, `-Dvespera.bench=true`, chunk sizes `16384`, `65536`, `262144` via `-Dvespera.streaming.chunkBytes=<n>`.
- JVM protocol: 3 Gradle/JVM invocations per side per benchmark; discard run 1 as cold; report median of runs 2–3 and retain both raw values above.
- Gradle invocation rule: every Gradle call used `--console=plain --no-daemon`; benchmark runs also used `--rerun-tasks` after Gradle's up-to-date check suppressed repeated benchmark execution.
- BEFORE `CARGO_TARGET_DIR` isolation: all BEFORE Cargo commands used `C:\Users\owjs3\Desktop\projects\vespera-before-bench\target-isolated`, so the main repo `target/` was never shared with the worktree.
- BEFORE cdylib evidence: isolated build produced `C:\Users\owjs3\Desktop\projects\vespera-before-bench\target-isolated\release\rust_jni_demo.dll`, length `1,774,592`, timestamp `2026-06-11 17:21:52 UTC`; because the Gradle plugin reads `target/release`, the DLL was copied to the worktree-local `target\release\rust_jni_demo.dll`, then bundled as `examples\rust-jni-demo\java\demo-app\build\resources\main\native\windows-x86_64\rust_jni_demo.dll`, length `1,774,592`, timestamp `2026-06-11 17:27:02 UTC`.
- AFTER cdylib evidence: main build produced `C:\Users\owjs3\Desktop\projects\vespera\target\release\rust_jni_demo.dll`, length `1,521,664`, timestamp `2026-06-11 14:35:03 UTC`; Gradle bundled `examples\rust-jni-demo\java\demo-app\build\resources\main\native\windows-x86_64\rust_jni_demo.dll`, length `1,521,664`, timestamp `2026-06-11 17:30:38 UTC`.
- Bridge versions: Maven local had both `kr/devfive/vespera-bridge/0.1.1` and `kr/devfive/vespera-bridge/0.2.0`. BEFORE `demo-app` was patched to `bridgeVersion.set("0.1.1")`; AFTER already pins `0.2.0`.
- BEFORE route support: the benchmark files did not exist at `6242533`, and the streaming benchmark's target route `POST /echo/stream` also did not exist. The throwaway worktree backported the current streaming echo route only to keep the throughput benchmark measuring JNI transport rather than route availability. Main production code was not changed.
- API availability: AFTER's `direct_pooled` / direct `ByteBuffer` path measures an API that did not exist BEFORE. The BEFORE gap is therefore recorded as `N/A`, and that missing path is part of the measured improvement unlocked by the v0.2.0 break.

### Verbatim backport diff between AFTER bench files and BEFORE-patched bench files

```diff
diff --git a/examples/rust-jni-demo/java/demo-app/src/test/java/kr/go/demo/SmallRequestLatencyBenchTest.java "b/..\\vespera-before-bench\\examples\\rust-jni-demo\\java\\demo-app\\src\\test\\java\\kr\\go\\demo\\SmallRequestLatencyBenchTest.java"
index 3327283..785f254 100644
--- a/examples/rust-jni-demo/java/demo-app/src/test/java/kr/go/demo/SmallRequestLatencyBenchTest.java
+++ "b/..\\vespera-before-bench\\examples\\rust-jni-demo\\java\\demo-app\\src\\test\\java\\kr\\go\\demo\\SmallRequestLatencyBenchTest.java"
@@ -6,7 +6,6 @@ import com.devfive.vespera.bridge.VesperaBridge;
 import java.io.ByteArrayInputStream;
 import java.io.IOException;
 import java.io.OutputStream;
-import java.nio.ByteBuffer;
 import java.util.Map;
 import java.util.concurrent.CompletableFuture;
 import java.util.concurrent.TimeUnit;
@@ -18,16 +17,8 @@ import org.junit.jupiter.api.condition.EnabledIfSystemProperty;
  * E2E small-request latency benchmark through the REAL JNI boundary —
  * quantifies what {@code vespera.bridge.dispatch-mode=smart} buys for
  * the requests it targets (small bounded idempotent), by comparing the
- * three dispatch modes on the same tiny {@code GET /health} round-trip:
- *
- * <ul>
- *   <li>{@code SYNC} — {@code encodeRequest} → {@code dispatchBytes}
- *       → {@code decodeResponse} (two JNI array copies)</li>
- *   <li>{@code DIRECT} — {@code dispatchDirectPooled} fast path
- *       (pooled direct buffers, no Java heap arrays)</li>
- *   <li>{@code BIDIRECTIONAL_STREAMING} — the autoconfigured default
- *       ({@code dispatchFullStreamingWithHeader})</li>
- * </ul>
+ * dispatch modes available in the 0.1.1 bridge on the same tiny
+ * {@code GET /health} round-trip.
  *
  * <p>Gated behind {@code -Dvespera.bench=true} so normal test runs and
  * CI skip it:
@@ -69,15 +60,6 @@ class SmallRequestLatencyBenchTest {
         return VesperaBridge.decodeResponse(VesperaBridge.dispatchBytes(wire)).status();
     }
 
-    private static int directOnce() {
-        ByteBuffer resp =
-                VesperaBridge.dispatchDirectPooled(null, "GET", "/health", null, HEADERS, null, true);
-        // Consume like the controller does: header region must be parsed.
-        byte[] out = new byte[resp.remaining()];
-        resp.get(out);
-        return VesperaBridge.decodeResponse(out).status();
-    }
-
     private static int streamingOnce() throws IOException {
         byte[] wireHeader = VesperaBridge.encodeRequestHeader("GET", "/health", null, HEADERS);
         CountingOutputStream sink = new CountingOutputStream();
@@ -137,7 +119,6 @@ class SmallRequestLatencyBenchTest {
     @Test
     void smallRequestLatencyByMode() throws IOException {
         long sync = measure("sync_dispatch_bytes", SmallRequestLatencyBenchTest::syncOnce);
-        long direct = measure("direct_pooled", SmallRequestLatencyBenchTest::directOnce);
         long respStreaming =
                 measure(
                         "response_streaming_only",
@@ -149,12 +130,8 @@ class SmallRequestLatencyBenchTest {
                         "async_completable_future",
                         SmallRequestLatencyBenchTest::asyncOnce);
         System.out.printf(
-                "VESPERA_BENCH summary direct_vs_streaming=%.2fx direct_vs_sync=%.2fx"
-                        + " resp_only_vs_bidi=%.2fx async_vs_sync=%.2fx async_vs_direct=%.2fx%n",
-                (double) streaming / direct,
-                (double) sync / direct,
+                "VESPERA_BENCH summary resp_only_vs_bidi=%.2fx async_vs_sync=%.2fx%n",
                 (double) streaming / respStreaming,
-                (double) async / sync,
-                (double) async / direct);
+                (double) async / sync);
     }
 }

--- StreamingThroughputBenchTest.java diff ---
```

`StreamingThroughputBenchTest.java` had no source-level diff after copying it into the BEFORE worktree; its bridge methods existed in 0.1.1. The separate route backport described above was required because `POST /echo/stream` was not present at `6242533`.

## Deferred

Text-envelope path optimization is intentionally deferred. The binary wire fast path covers the dominant JNI use case: Spring/Java proxying real request and response bytes through the length-prefixed binary envelope without base64 or domain JSON parsing. The text-envelope path is a niche direct-API fallback rather than the JNI hot path, so this perf series focuses on byte-array region copies, cached JNI method lookups, direct buffers, and binary streaming first.

## Traps encountered and resolution

- `dispatchDirectPooled` was absent from 0.1.1: dropped `direct_pooled` on the BEFORE side and reported it as `N/A` with the API-gap footnote.
- `POST /echo/stream` was absent from `6242533`: backported the current streaming echo route only in the throwaway worktree so streaming throughput compares JNI transport rather than a 404/route mismatch.
- Gradle repeated test invocations were `UP-TO-DATE`: reran the benchmark protocol with `--rerun-tasks` while retaining `--console=plain --no-daemon`.
- The Gradle plugin bundles from `target/release`: BEFORE Cargo still built with isolated `CARGO_TARGET_DIR=...\target-isolated`, then the built DLL was copied into the worktree-local `target/release` path before Gradle bundling.
- GPG signing blocked the throwaway worktree commit: the first commit attempt timed out in GPG; the ephemeral worktree commits were created with per-command `git -c commit.gpgsign=false`, with no config change and no push.

## Re-gate: async attach optimization

Decision: **keep the async completion daemon-attach optimization**. `jni` 0.22.4 source shows `JavaVM::attach_current_thread` is already a permanent cached attachment (`java_vm.rs` lines 450-469), while `attach_current_thread_for_scope` is the scoped detach-on-return API (`java_vm.rs` lines 500-513). The crate does not expose a safe daemon attachment helper and explicitly says daemon threads are not directly supported (`java_vm.rs` lines 1027-1047), so the async completion path uses JNI 1.4's raw `AttachCurrentThreadAsDaemon` entry from `jni-sys` and caches its `JNIEnv` per Tokio worker thread, with a per-completion local frame to prevent local-reference accumulation.

Protocol: same 3 JVM invocations; run 1 discarded as cold; retained value is the arithmetic midpoint of runs 2-3. Gate metric is `async_completable_future`.

| side | run | `sync_dispatch_bytes` | `direct_pooled` | `response_streaming_only` | `bidirectional_streaming` | `async_completable_future` |
|---|---:|---:|---:|---:|---:|---:|
| CURRENT | 1 (discarded) | 3,579 | 2,755 | 7,518 | 21,992 | 28,651 |
| CURRENT | 2 | 3,409 | 3,299 | 6,420 | 22,845 | 24,045 |
| CURRENT | 3 | 3,188 | 2,462 | 6,563 | 17,237 | 21,466 |
| DAEMON | 1 (discarded) | 2,890 | 2,265 | 6,119 | 16,315 | 20,270 |
| DAEMON | 2 | 2,987 | 2,188 | 6,307 | 18,893 | 21,027 |
| DAEMON | 3 | 3,158 | 2,263 | 6,242 | 18,002 | 21,921 |

| metric | CURRENT median ns/op | DAEMON median ns/op | improvement |
|---|---:|---:|---:|
| `async_completable_future` | 22,756 | 21,474 | **1,282 ns/op faster** (-5.6%) |

The measured win is above the **100 ns/op** keep gate. Follow-up review found that the daemon-attached Tokio worker must explicitly clear pending Java exceptions after every completion callback because it no longer gets jni-rs scoped-detach cleanup. The implementation now clears pending exceptions after callback success, callback error, and callback unwind while preserving the callback return/error. A targeted regression guard, `AsyncDispatchExceptionHygieneTest.throwingFutureCompleteDoesNotPoisonNextAsyncCompletion`, first forces `CompletableFuture.complete()` to throw and then asserts a normal `dispatchAsync` still completes with status 200; it failed before the cleanup with a timeout and passes after the fix. A single post-fix sanity bench run measured `async_completable_future` at **16,107 ns/op** (informational only; not a replacement for the 3-JVM gate). Verification also passed `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test --workspace`, `cargo build -p rust-jni-demo --release`, and the full `:demo-app:test` Gradle suite (including `StreamingClosureStressTest` and the new hygiene guard).
