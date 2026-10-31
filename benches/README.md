# Compile-time benchmarks

A reproducible harness for measuring the **compile-time cost of vespera's
proc-macros** (`vespera!`, `schema_type!`, `#[derive(Schema)]`). This is the
compile-time analogue of the runtime criterion benches
(`crates/vespera_inprocess/benches/dispatch.rs`) and the deterministic
allocation gate (`crates/vespera_inprocess/tests/alloc_budget.rs`).

| Crate | Role |
|---|---|
| [`macro-compile-bench`](./macro-compile-bench) | **Fixture** — a deliberately schema- and cross-reference-heavy `vespera!` app. Hub schemas (`User`, `Product`, `Order`) are referenced by many routes, so the per-reference schema-generation cost that macro optimizations target is exercised. |
| [`compile-bench-runner`](./compile-bench-runner) | **Harness** — a std-only orchestrator that measures the `macro_expand_crate` rustc pass and reports min/median/mean/stddev with baseline A/B comparison. |

## What it measures

The harness extracts the **`macro_expand_crate`** pass from `rustc -Z
time-passes`, which **isolates macro expansion** from the rest of compilation
(name resolution, type-check, codegen, LTO). This is the right signal for
proc-macro work: optimizing `vespera_macro` only changes expansion time, which
is a small fraction of a crate's total build, so measuring total wall-clock
would bury the change under noise.

It runs on a **stable** toolchain via `RUSTC_BOOTSTRAP=1` (no nightly needed),
so it works in CI.

## Usage

```bash
# Save a baseline on the current (e.g. unmodified) macro code:
cargo run -p compile-bench-runner -- --runs 8 --save-baseline before

#  ... make changes to crates/vespera_macro ...

# Compare against the baseline:
cargo run -p compile-bench-runner -- --runs 8 --baseline before
```

Options:

| Flag | Default | Meaning |
|---|---|---|
| `--target <CRATE>` | `macro-compile-bench` | crate to measure (must be a lib that expands the macros) |
| `--pass <NAME>` | `macro_expand_crate` | which `-Z time-passes` pass to extract |
| `--runs <N>` | `8` | measured iterations |
| `--save-baseline <X>` | — | write samples to `compile-bench-runner/baselines/<X>.txt` |
| `--baseline <X>` | — | compare current run against `baselines/<X>.txt` |

You can also point it at the bundled example for a heavier, real-world workload:

```bash
cargo run -p compile-bench-runner -- --target axum-example --runs 8 --save-baseline ax
```

## Methodology & noise

- Each iteration runs `cargo clean -p <target>` to force a **full
  re-expansion**, then `cargo rustc … -- -Z time-passes`.
- Compile time has only **positive** noise (a busy machine only ever *adds*
  time), so **`min` is the robust point estimate**; median/mean/sd are also
  reported. Gross outliers (> 3× median, e.g. antivirus/FS hiccups on Windows)
  are dropped before stats.
- The fixture's `macro_expand_crate` is stable to within a few percent
  (~3–4% sd). The A/B verdict requires a change to exceed run-to-run noise
  (≥ 2%) before reporting `IMPROVED` / `REGRESSED`.
- **Run on a quiet machine.** Close other heavy processes; the harness reports
  the relative stddev so you can judge whether a measurement was clean.

> Note: a stale baseline measured under different machine load can produce a
> false delta. For a rigorous before/after, measure both arms **back-to-back**
> in one sitting (save baseline → change → compare) rather than comparing
> against a baseline captured hours earlier.

## Baselines are local

`compile-bench-runner/baselines/*.txt` hold absolute timings that are specific
to the machine/toolchain that produced them, so they are **git-ignored**.
Capture your own before/after on the same machine in one session.

## Scale / extreme-input tooling

Micro benches hide per-op costs that compound (or turn non-linear) in large
applications, so there are two opt-in scale harnesses. Neither is part of the PR
bench gate (`bench.yml` runs only `--bench dispatch`) and neither joins
`cargo build --workspace`.

### Macro build cost vs. number of routes

`scripts/gen-stress-app.ps1 -Routes N` emits a standalone crate (own `[workspace]`,
under `target/stress-app/`) with N routes, 50 handlers per file, plus per-route
`#[derive(Schema)]` request/response structs, a 12-variant enum every 10th route, a
40-field struct every 25th, a self-referential node every 30th, an 8-deep nesting
chain, the generic `Page<T>` and a `schema_type!` derivation every 20th.

`scripts/measure-stress.ps1 -Sizes 10,100,500,2000 -Runs 3` reports, per N: clean
rebuild wall time, the `macro_expand_crate` pass, peak rustc working set (sampled every
25 ms), an incremental rebuild, a `vespera!` cache-hit rebuild and `openapi.json` size,
then the log-log slope (1.0 = linear). `-Control -Keep schema,route[,axum]` strips
vespera pieces (or swaps `vespera!` for a hand-written axum router) so the cost vespera
itself adds can be attributed; `VESPERA_PROFILE=1` prints `vespera!`'s per-stage timings.

### Runtime scale ladders

`cargo bench -p vespera_inprocess --bench extreme` runs size ladders for route tables
(1k-50k routes), request/response headers (10-10k), header values (ASCII / Unicode /
escape-heavy up to the 1 MiB wire-header cap), 422 envelopes (10-100k entries, crossing
the 64 KiB hoist cap), long paths/queries, buffered bodies (1-100 MiB), response and
bidirectional streaming (16-1000 MiB, peak heap printed by a counting allocator),
1k-100k concurrent dispatches and 100-10k registered apps. Every case asserts the status
it intends to measure. Read each ladder as ns per unit and check that figure is flat.
`VESPERA_EXTREME=1` adds the heaviest rungs.
