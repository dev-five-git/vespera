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

pub use crate::validated::validation_error_body;

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
/// caught first by [`range_unsatisfiable`]); a float type keeps the value,
/// except that an `f32` steps the nearest `f32` to the inside of the bound
/// when it rounds across it.
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
    /// The value of the integer-literal bound `literal`, loosened to the
    /// documented `bound` (`f64`) when that is looser: past 2^53 the OpenAPI
    /// document can only carry the rounded value, and a client sending that
    /// advertised bound must not be rejected. `lower` names the bound's side
    /// (`minimum` when `true`).
    fn exact_within(literal: Self::Literal, bound: f64, lower: bool) -> Self;
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
///
/// Generic over `garde`'s range `Bounds` like the `range` rule itself, so the
/// field may be an `Option` (any depth, also behind an alias such as
/// `type MaybeCount = Option<u32>`): the bound type is its `Bounds::Size`.
/// Like that rule, an absent value (`None` at any layer) passes: an omitted
/// optional property is valid whatever its bounds.
pub fn range_unsatisfiable<T>(value: &T, min: Option<f64>, max: Option<f64>) -> Option<garde::Error>
where
    T: garde::rules::range::Bounds,
    T::Size: RangeBound,
{
    // The inverted bounds `[MAX, MIN]` reject every present value and pass
    // only `None`, so this is "is a value present" for any `Option` depth.
    if value.validate_bounds(T::MAX, T::MIN).is_ok() {
        return None;
    }
    if let Some(min) = min.filter(|&min| !<T::Size as RangeBound>::admits_min(min)) {
        return Some(garde::Error::new(format!("lower than {min}")));
    }
    max.filter(|&max| !<T::Size as RangeBound>::admits_max(max))
        .map(|max| garde::Error::new(format!("greater than {max}")))
}

/// The error for a non-finite value of a float field with a
/// `#[schema(minimum / maximum)]` bound, checked before `garde`'s `range`
/// rule: every comparison with `NaN` is false, so that rule admits `NaN`
/// whatever the bounds (`str::parse::<f64>` behind `Query` / `Form` / `Path`
/// accepts `NaN` and `inf`), while the document's `type: number` excludes it.
///
/// Generic over `garde`'s range `Bounds` like [`range_unsatisfiable`]: an
/// absent value (`None` at any layer) passes, an integer value never fails.
pub fn non_finite<T: garde::rules::range::Bounds>(value: &T) -> Option<garde::Error> {
    // `NaN` is unordered even against itself (`Option`'s order compares the
    // inner value); an infinity lies outside the type's finite `[MIN, MAX]`.
    #[allow(clippy::eq_op)]
    let nan = value.partial_cmp(value).is_none();
    (nan || value.validate_bounds(T::MIN, T::MAX).is_err())
        .then(|| garde::Error::new("not a finite number"))
}

macro_rules! integer_range_bound {
    ($($ty:ty),*) => {$(
        impl RangeBound for $ty {
            type Literal = Self;
            fn exact(literal: Self) -> Self {
                literal
            }
            fn exact_within(literal: Self, bound: f64, lower: bool) -> Self {
                if lower {
                    Ord::min(literal, <Self as RangeBound>::min(bound))
                } else {
                    Ord::max(literal, <Self as RangeBound>::max(bound))
                }
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
    // A float keeps the literal's own value: the documented `f64` is that
    // same value rounded, which the literal already is.
    fn exact_within(literal: i128, _bound: f64, _lower: bool) -> Self {
        Self::exact(literal)
    }
    fn min(bound: f64) -> Self {
        bound
    }
    fn max(bound: f64) -> Self {
        bound
    }
}

/// Steps the nearest `f32` `value` to the inside of the documented `f64`
/// `bound`: a lower bound is never undershot, an upper bound never overshot.
/// Without the step a bound such as `16777217` or `0.099_999_997_01` rounds to
/// the wrong side of the document, the same error the integer impls avoid by
/// rounding inward.
fn inward_f32(value: f32, bound: f64, lower: bool) -> f32 {
    if lower && f64::from(value) < bound {
        value.next_up()
    } else if !lower && f64::from(value) > bound {
        value.next_down()
    } else {
        value
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
    // The looser of the literal and the documented `bound`, as for the
    // integer impls, rounded to the nearest `f32` and then stepped inward
    // against that value.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn exact_within(literal: i128, bound: f64, lower: bool) -> Self {
        let literal = literal as f64;
        let looser = if lower {
            literal.min(bound)
        } else {
            literal.max(bound)
        };
        inward_f32(looser as Self, looser, lower)
    }
    #[allow(clippy::cast_possible_truncation)]
    fn min(bound: f64) -> Self {
        inward_f32(bound as Self, bound, true)
    }
    #[allow(clippy::cast_possible_truncation)]
    fn max(bound: f64) -> Self {
        inward_f32(bound as Self, bound, false)
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
        // `Option` layers range as their innermost type, like `garde`'s rule.
        assert!(super::range_unsatisfiable(&Some(Some(0u8)), Some(300.5), None).is_some());
        assert!(super::range_unsatisfiable(&None::<u8>, None, Some(2.5)).is_none());
        // An absent value passes an unsatisfiable bound, like `garde`'s rule.
        assert!(super::range_unsatisfiable(&None::<u8>, Some(300.5), None).is_none());
        assert!(super::range_unsatisfiable(&Some(None::<u32>), None, Some(-0.5)).is_none());
        assert!(super::range_unsatisfiable(&Some(u8::MAX), Some(300.5), None).is_some());
        assert_eq!(
            super::range_unsatisfiable(&0u8, Some(300.5), None).map(|e| e.to_string()),
            Some("lower than 300.5".to_owned())
        );
        assert_eq!(
            super::range_unsatisfiable(&0u32, None, Some(-0.5)).map(|e| e.to_string()),
            Some("greater than -0.5".to_owned())
        );
        assert!(super::non_finite(&f64::NAN).is_some());
        assert!(super::non_finite(&Some(Some(f32::NEG_INFINITY))).is_some());
        assert!(super::non_finite(&None::<f64>).is_none());
        assert!(super::non_finite(&f64::MAX).is_none() && super::non_finite(&u8::MAX).is_none());
        assert!((<f64 as RangeBound>::exact(-3) + 3.0).abs() < f64::EPSILON);
        assert!((<f32 as RangeBound>::exact(100) - 100.0).abs() < f32::EPSILON);
    }

    /// An `f32` bound never lands on the outside of the documented `f64`
    /// bound: the nearest `f32` steps to the inward neighbour when it rounds
    /// across the bound (`16777217` and `0.099_999_997_01` are not `f32` values).
    #[test]
    fn f32_bounds_round_inward() {
        // 16777216 is the nearest f32 to the minimum 16777217: below it, so
        // the bound steps up to 16777218. The maximum keeps 16777216.
        assert_eq!(
            <f32 as RangeBound>::exact_within(16_777_217, 16_777_217.0, true),
            16_777_218.0
        );
        assert_eq!(
            <f32 as RangeBound>::exact_within(16_777_217, 16_777_217.0, false),
            16_777_216.0
        );
        assert_eq!(<f32 as RangeBound>::min(16_777_217.0), 16_777_218.0);
        // 0.09999999404 is the nearest f32 to the minimum 0.099_999_997_01, below
        // it, so the bound steps up to 0.1f32 (0.100000001490116).
        let minimum = <f32 as RangeBound>::min(0.099_999_997_01);
        assert_eq!(minimum, f32::from_bits(0x3DCC_CCCD));
        assert!(f64::from(minimum) >= 0.099_999_997_01);
        // Above the maximum 0.100_000_000_1, 0.1f32 steps down to 0.09999999404.
        let maximum = <f32 as RangeBound>::max(0.100_000_000_1);
        assert_eq!(maximum, f32::from_bits(0x3DCC_CCCC));
        assert!(f64::from(maximum) <= 0.100_000_000_1);
        // A bound that is exact in f32 is kept as it is.
        assert_eq!(<f32 as RangeBound>::max(2.5), 2.5);
        assert_eq!(<f32 as RangeBound>::exact_within(7, 7.0, true), 7.0);
    }

    /// An integer-literal bound keeps the literal, unless the documented
    /// value is looser (past 2^53 the document rounds it).
    #[test]
    fn exact_bounds_loosen_to_the_documented_value() {
        let past_2_pow_53 = 9_007_199_254_740_993_u64;
        let documented = 9_007_199_254_740_992.0;
        assert_eq!(
            <u64 as RangeBound>::exact_within(past_2_pow_53, documented, true),
            9_007_199_254_740_992
        );
        assert_eq!(
            <u64 as RangeBound>::exact_within(past_2_pow_53, documented, false),
            past_2_pow_53
        );
        assert_eq!(<i64 as RangeBound>::exact_within(5, 3.0, true), 3);
        assert_eq!(<i64 as RangeBound>::exact_within(3, 5.0, true), 3);
        assert_eq!(<u8 as RangeBound>::exact_within(5, 7.0, false), 7);
        assert_eq!(<u8 as RangeBound>::exact_within(9, 7.0, false), 9);
        assert!((<f64 as RangeBound>::exact_within(7, 0.0, true) - 7.0).abs() < f64::EPSILON);
        assert!((<f32 as RangeBound>::exact_within(7, 0.0, false) - 7.0).abs() < f32::EPSILON);
    }
}
