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

/// Compile-time check emitted by `#[route]` for the payload `U` of every
/// `Validated<E<U>>` / `ValidatedWith<C, E<U>>` argument.
///
/// Implemented for every `garde::Validate` type, so it never constrains
/// anything the extractor itself does not; it exists only so a missing impl is
/// reported with vespera's message (attached to a trait vespera owns) instead
/// of an opaque `Handler` trait-bound error at the router.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be validated by `Validated` / `ValidatedWith`: it does not implement `garde::Validate`",
    label = "this payload type has no `garde::Validate` impl",
    note = "`#[derive(Schema)]` generates `garde::Validate` only for a struct with named fields and at least one runtime `#[schema(...)]` rule (e.g. `#[schema(min_length = 1)]`); enums, tuple structs and rule-less structs get none",
    note = "add a `#[schema(...)]` rule, `#[derive(garde::Validate)]` the type yourself, or drop the `Validated` wrapper and take the plain extractor"
)]
pub trait AssertValidate {}

impl<T: garde::Validate + ?Sized> AssertValidate for T {}
