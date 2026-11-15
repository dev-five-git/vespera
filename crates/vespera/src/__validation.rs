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
/// excludes — saturating at the type's range (a bound no value reaches is
/// caught first by [`range_unsatisfiable`]); a float type keeps the value.
///
/// [`RangeBound::exact`] carries an integer-literal bound: an integer target
/// takes the literal in its own type (so an out-of-range literal is still the
/// compiler's `overflowing_literals` error), a float target takes it as an
/// `i128` (an unsuffixed `0` never infers as `f64`).
pub trait RangeBound: Sized {
    /// The type an integer-literal bound is written in for this target.
    type Literal;
    /// The value of the lower bound `bound`.
    fn min(bound: f64) -> Self;
    /// The value of the upper bound `bound`.
    fn max(bound: f64) -> Self;
    /// The value of the integer-literal bound `literal`.
    fn exact(literal: Self::Literal) -> Self;
    /// Whether some value of the type satisfies the lower bound `bound`
    /// (an integer type cannot reach `minimum = 300.5` on a `u8`, which
    /// [`RangeBound::min`] would saturate to an admitting `255`).
    fn admits_min(bound: f64) -> bool;
    /// Whether some value of the type satisfies the upper bound `bound`.
    fn admits_max(bound: f64) -> bool;
}

/// The error for a fractional `minimum` / `maximum` no value of the field's
/// (aliased) type can satisfy, checked before `garde`'s `range` rule so the
/// saturated bound never admits a value the document excludes; `None` when
/// both bounds are reachable.
pub fn range_unsatisfiable<T: RangeBound>(
    _value: &T,
    min: Option<f64>,
    max: Option<f64>,
) -> Option<garde::Error> {
    if let Some(min) = min.filter(|&min| !T::admits_min(min)) {
        return Some(garde::Error::new(format!("lower than {min}")));
    }
    max.filter(|&max| !T::admits_max(max))
        .map(|max| garde::Error::new(format!("greater than {max}")))
}

macro_rules! integer_range_bound {
    ($($ty:ty),*) => {$(
        impl RangeBound for $ty {
            type Literal = Self;
            fn exact(literal: Self) -> Self {
                literal
            }
            // `MAX as f64 + 1.0` is exact up to 32 bits and rounds to the
            // power of two `MAX + 1` beyond, so `<` is the exact test.
            #[allow(clippy::cast_precision_loss, clippy::cast_lossless)]
            fn admits_min(bound: f64) -> bool {
                bound.ceil() < Self::MAX as f64 + 1.0
            }
            // `MIN` is zero or a power of two: exact as `f64`.
            #[allow(clippy::cast_precision_loss, clippy::cast_lossless)]
            fn admits_max(bound: f64) -> bool {
                bound.floor() >= Self::MIN as f64
            }
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
    type Literal = i128;
    fn admits_min(_bound: f64) -> bool {
        true
    }
    fn admits_max(_bound: f64) -> bool {
        true
    }
    #[allow(clippy::cast_precision_loss)]
    fn exact(literal: i128) -> Self {
        literal as Self
    }
    fn min(bound: f64) -> Self {
        bound
    }
    fn max(bound: f64) -> Self {
        bound
    }
}

impl RangeBound for f32 {
    type Literal = i128;
    fn admits_min(_bound: f64) -> bool {
        true
    }
    fn admits_max(_bound: f64) -> bool {
        true
    }
    #[allow(clippy::cast_precision_loss)]
    fn exact(literal: i128) -> Self {
        literal as Self
    }
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
        assert_eq!(<u8 as RangeBound>::exact(7), 7);
        assert!(<u8 as RangeBound>::admits_min(254.5));
        assert!(!<u8 as RangeBound>::admits_min(255.5));
        assert!(!<u32 as RangeBound>::admits_max(-0.5));
        assert!(<i8 as RangeBound>::admits_max(-127.5));
        assert!(!<i8 as RangeBound>::admits_max(-128.5));
        assert!(!<u64 as RangeBound>::admits_min(
            18_446_744_073_709_551_615.5
        ));
        assert!(<f64 as RangeBound>::admits_min(1e300) && <f64 as RangeBound>::admits_max(-1e300));
        assert!(<f32 as RangeBound>::admits_min(1e300) && <f32 as RangeBound>::admits_max(-1e300));
        assert!(super::range_unsatisfiable(&0u8, Some(1.5), Some(2.5)).is_none());
        assert_eq!(
            super::range_unsatisfiable(&0u8, Some(300.5), None).map(|e| e.to_string()),
            Some("lower than 300.5".to_owned())
        );
        assert_eq!(
            super::range_unsatisfiable(&0u32, None, Some(-0.5)).map(|e| e.to_string()),
            Some("greater than -0.5".to_owned())
        );
        assert!((<f64 as RangeBound>::exact(-3) + 3.0).abs() < f64::EPSILON);
        assert!((<f32 as RangeBound>::exact(100) - 100.0).abs() < f32::EPSILON);
    }
}
