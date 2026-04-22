# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@AGENTS.md

## Repository Shape

Vespera is a **hybrid monorepo** with two workspaces living side-by-side at the repo root:

| Workspace | Manager | Members | Purpose |
|-----------|---------|---------|---------|
| Cargo (`Cargo.toml`) | cargo | `crates/*`, `examples/*` (excluding `examples/java-jni-demo`) | OpenAPI engine, proc-macros, JNI bridge |
| Bun (`package.json`) | bun | `apps/*` | Marketing/docs site + admin panel (Next.js) |

Both live at the root — `bun run ...` operates on the Node side; `cargo ...` on the Rust side. Many root scripts deliberately cross the boundary (e.g., `prelint` runs `cargo clippy/fmt/check` **before** oxlint runs on JS). See `AGENTS.md` for crate-level detail.

## Common Commands (Root)

```bash
# Rust side
cargo build                           # Build all crates
cargo test --workspace                # All Rust tests
cargo test -p vespera_macro           # One crate
cargo test --test <name> -- <filter>  # Single integration test
cargo tarpaulin --out stdout          # Coverage (run via `bun run posttest`)

# Lint / format (order matters — `prelint` hook runs Rust FIRST)
bun run lint                          # oxlint over JS/TS (runs after prelint → cargo clippy+fmt+check)
bun run lint:fix                      # oxlint --fix (prelint:fix runs cargo clippy --fix + fmt first)

# Front-end workspace
bun run dev                           # Runs `dev` in every apps/* (Next.js dev servers)
bun run build                         # Builds apps/front and apps/admin
cd apps/front && bun dev              # Single-app dev (preferred over -F flag, per devfive-frontend skill)

# Tests (Bun side)
bun test                              # Root runs bun test + tarpaulin via posttest hook

# Release tooling
bun run changepacks                   # Version bumps via @changepacks/cli
```

> **`prelint` gotcha:** `bun run lint` triggers `cargo clippy -- -D warnings && cargo fmt --check && cargo check` first. Any Rust warning fails the JS lint. Run `bun run lint:fix` (which chains `cargo clippy --fix && cargo fmt`) to auto-resolve both sides.

## Frontend (`apps/front`)

Next.js 16 App Router + React 19 + @devup-ui/react (build-time CSS-in-JS). Theme tokens live in `apps/front/devup.json` and use the `$token` syntax in JSX props only.

- `apps/front/src/app/` contains **only** `layout.tsx` and `page.tsx` — all other components belong in `src/components/` (per devfive-frontend conventions).
- `src/api.ts` is generated/edited via `@devup-api/fetch`; it currently contains a placeholder `/users/users` call that fails typecheck — a known scaffolding leftover, not a regression.
- Styling: use devup-ui shorthand props (`bg`, `p`, `w`, `_hover`, `[mobile,null,pc]` responsive arrays). Never `style={{...}}` or Tailwind. See `~/.claude/skills/devup-ui/SKILL.md` via the `/devup-ui` skill.

## Rust × Java Boundary

The JNI integration (`crates/vespera_jni` → `libs/vespera-bridge/` Java lib) is load-bearing for `examples/rust-jni-demo`. When touching:

- `vespera_inprocess` owns transport-agnostic dispatch (no JNI deps).
- `vespera_jni` is a thin layer depending on `vespera_inprocess` + `jni` + `tokio/rt-multi-thread`.
- User code depends on `vespera` only, with `features = ["jni"]` — never `vespera_jni` directly. Breaking this invariant is an AGENTS.md-listed anti-pattern.

The Java package `com.devfive.vespera.bridge` is **fixed** because the JNI symbol name is derived from it. Renaming it breaks the native load.

## Pre-Commit (Husky)

`bun run prepare` installs husky. Commits trigger whatever lives in `.husky/` — typically a `lint` pass. Never bypass with `--no-verify`; fix the underlying Rust or oxlint finding.

## Where Tests Live

| Concern | Location |
|---------|----------|
| Macro integration tests | `crates/vespera_macro/tests/` (+ `insta` snapshots) |
| Core unit tests | `crates/vespera_core/src/**` inline `#[cfg(test)]` |
| JNI end-to-end | `examples/rust-jni-demo` (Rust + Java + Gradle) |
| Front tests | `apps/front/src/__tests__/` (bun test + bun-test-env-dom) |

Snapshot tests use `insta` — run `cargo insta review` to accept drifts.
