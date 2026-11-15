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

/// Conversion of a fractional `#[schema(minimum / maximum)]` bound to a field
/// type the macro cannot see through (`type Count = u32`).
///
/// An integer type rounds inward — `minimum = 0.5` on a `u32` is `1`, never a
/// truncated `0` that would accept a value the document's `minimum: 0.5`
/// excludes — saturating at the type's range; a float type keeps the value.
pub trait RangeBound: Sized {
    /// The value of the lower bound `bound`.
    fn min(bound: f64) -> Self;
    /// The value of the upper bound `bound`.
    fn max(bound: f64) -> Self;
}

macro_rules! integer_range_bound {
    ($($ty:ty),*) => {$(
        impl RangeBound for $ty {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            fn min(bound: f64) -> Self {
                bound.ceil() as Self
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            fn max(bound: f64) -> Self {
                bound.floor() as Self
            }
        }
    )*};
}

integer_range_bound!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

impl RangeBound for f64 {
    fn min(bound: f64) -> Self {
        bound
    }
    fn max(bound: f64) -> Self {
        bound
    }
}

impl RangeBound for f32 {
    #[allow(clippy::cast_possible_truncation)]
    fn min(bound: f64) -> Self {
        bound as Self
    }
    #[allow(clippy::cast_possible_truncation)]
    fn max(bound: f64) -> Self {
        bound as Self
    }
}

#[cfg(test)]
mod tests {
    use super::RangeBound;

    /// Integers round inward (saturating), floats keep the bound.
    #[test]
    fn range_bounds_round_integers_inward() {
        assert_eq!(<u32 as RangeBound>::min(0.5), 1);
        assert_eq!(<u32 as RangeBound>::max(2.5), 2);
        assert_eq!(<i8 as RangeBound>::min(-0.5), 0);
        assert_eq!(<i8 as RangeBound>::max(-0.5), -1);
        assert_eq!(<u8 as RangeBound>::max(300.5), u8::MAX);
        assert_eq!(<u16 as RangeBound>::min(-3.5), 0);
        assert!((<f64 as RangeBound>::min(0.5) - 0.5).abs() < f64::EPSILON);
        assert!((<f64 as RangeBound>::max(2.5) - 2.5).abs() < f64::EPSILON);
        assert!((<f32 as RangeBound>::min(0.5) - 0.5).abs() < f32::EPSILON);
        assert!((<f32 as RangeBound>::max(2.5) - 2.5).abs() < f32::EPSILON);
    }
}
