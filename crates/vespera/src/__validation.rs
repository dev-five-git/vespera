//! Private re-export module used by `vespera_macro`-generated code.
//!
//! When the `validation` feature is enabled, `#[derive(vespera::Schema)]`
//! emits an `impl ::vespera::__validation::garde::Validate` block.  The
//! impl body calls `::vespera::__validation::garde::rules::*::apply` and
//! constructs paths via `::vespera::__validation::garde::util::nested_path!`.
//!
//! Going through this `__validation` facade keeps two guarantees:
//!
//! 1. **Users never name `garde`.**  A single
//!    `vespera = { features = ["validation"] }` is all the user needs;
//!    the macro never produces `::garde::...` paths in user code, so the
//!    user's `Cargo.toml` doesn't need a `garde` dependency.
//!
//! 2. **Reversibility.**  Should we ever swap the runtime validator (or
//!    build our own), only this module changes — the emitted call sites
//!    stay the same.  Macro expansions in user crates are insulated from
//!    the swap.
//!
//! This module is `pub` because the macro must emit absolute paths
//! through it, but it lives behind `#[doc(hidden)]` and is not part of
//! the stable surface.  External callers must use [`crate::Validated`].

pub use ::garde;
