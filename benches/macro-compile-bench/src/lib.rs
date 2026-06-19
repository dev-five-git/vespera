//! Macro compile-time benchmark **fixture**.
//!
//! A deliberately schema- and cross-reference-heavy `vespera!` application
//! whose sole purpose is to give the [`compile-bench-runner`] harness a
//! stable, representative proc-macro expansion workload to measure. Hub
//! schemas (`User`, `Product`, `Order`) are referenced by many routes so the
//! per-reference schema-generation cost — exactly what compile-time macro
//! optimizations target — is exercised.
//!
//! The harness measures the `macro_expand_crate` rustc pass of this crate's
//! `lib`, which isolates `vespera!` / `#[derive(Schema)]` expansion from the
//! rest of compilation (type-check, codegen, LTO).
//!
//! This is benchmark scaffolding, not a production example; lints are relaxed
//! (e.g. `ErrorBody` is referenced only from a `responses = [...]` attribute,
//! which does not count as an import use).
#![allow(clippy::all, clippy::pedantic, unused)]

pub mod models;
mod routes;

use vespera::{axum, vespera};

/// Expand `vespera!` over `src/routes/` — the call the compile-time harness
/// measures. No `openapi = ...` output is configured, so building this crate
/// performs the expansion without writing files.
#[must_use]
pub fn create_app() -> axum::Router {
    vespera!(title = "Macro Compile Bench", version = "1.0.0")
}
