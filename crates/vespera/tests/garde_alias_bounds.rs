//! A fractional `minimum` / `maximum` on a field whose type the macro cannot
//! see through (a type alias) rounds inward for an integer target — a
//! truncating `0.5 as u32` accepted `0` while the document says
//! `minimum: 0.5` — and keeps the value for a float target. An integer-literal
//! bound on a float alias compiles (an unsuffixed `0` never infers as `f64`).

#![cfg(feature = "validation")]

use ::vespera::__validation::garde::Validate;
use ::vespera::Schema;

type Count = u32;
type Ratio = f64;

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct Bounded {
    // `value_type` documents the alias (an alias is no `Schema` of its own);
    // the bounds still range over the field type the macro cannot resolve.
    #[schema(value_type = u32, minimum = 0.5, maximum = 2.5)]
    count: Count,
    #[schema(value_type = f64, minimum = 0.5, maximum = 2.5)]
    ratio: Ratio,
}

#[test]
fn fractional_bounds_on_aliases_round_inward_for_integers() {
    let valid = |count, ratio| Bounded { count, ratio }.validate().is_ok();
    assert!(!valid(0, 1.0), "0 < minimum 0.5");
    assert!(valid(1, 1.0));
    assert!(valid(2, 1.0));
    assert!(!valid(3, 1.0), "3 > maximum 2.5");
    assert!(!valid(1, 0.25), "0.25 < minimum 0.5");
    assert!(valid(1, 0.5));
    assert!(valid(1, 2.5));
    assert!(!valid(1, 2.75), "2.75 > maximum 2.5");
}

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct IntegerBounded {
    #[schema(value_type = f64, minimum = 0, maximum = 100)]
    ratio: Ratio,
    #[schema(value_type = f32, minimum = -1, maximum = 1)]
    small: f32Alias,
    #[schema(value_type = u32, minimum = 1, maximum = 3)]
    count: Count,
}

#[allow(non_camel_case_types)]
type f32Alias = f32;

#[test]
fn integer_bounds_on_float_aliases_compile_and_apply() {
    let valid = |ratio, small, count| {
        IntegerBounded {
            ratio,
            small,
            count,
        }
        .validate()
        .is_ok()
    };
    assert!(valid(0.0, 0.0, 1));
    assert!(valid(100.0, 1.0, 3));
    assert!(!valid(-0.5, 0.0, 1), "-0.5 < minimum 0");
    assert!(!valid(100.5, 0.0, 1), "100.5 > maximum 100");
    assert!(!valid(1.0, -1.5, 1), "-1.5 < minimum -1");
    assert!(!valid(1.0, 0.0, 0), "0 < minimum 1");
    assert!(!valid(1.0, 0.0, 4), "4 > maximum 3");
}

type Small = u8;

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct Unreachable {
    #[schema(value_type = u8, minimum = 300.5)]
    above: Small,
    #[schema(value_type = u32, maximum = -0.5)]
    below: Count,
}

/// A fractional bound no value of the aliased integer type reaches rejects
/// every value: the saturating conversion alone admitted `u8::MAX` for
/// `minimum = 300.5` and `0` for `maximum = -0.5`.
#[test]
fn unreachable_fractional_bounds_on_integer_aliases_reject_every_value() {
    let report = Unreachable {
        above: u8::MAX,
        below: 0,
    }
    .validate()
    .unwrap_err();
    let messages: Vec<String> = report
        .iter()
        .map(|(path, e)| format!("{path}: {e}"))
        .collect();
    assert_eq!(
        messages,
        ["above: lower than 300.5", "below: greater than -0.5"]
    );
}

type MaybeCount = Option<u32>;

macro_rules! grouped {
    ($t:ty) => {
        #[derive(Schema, serde::Deserialize)]
        #[allow(dead_code)]
        struct Grouped {
            #[schema(value_type = u32, minimum = 0.5)]
            count: $t,
        }
    };
}
grouped!(Option<u32>);

/// The guard ranges over the innermost type of every `Option` layer the
/// macro cannot unwrap (a second layer, an alias of an `Option`, a
/// `macro_rules!` `$t:ty` group) — it used to be a `RangeBound` E0277.
#[derive(Schema, serde::Deserialize)]
#[allow(dead_code, clippy::option_option)]
struct Layered {
    #[schema(value_type = u32, minimum = 0.5)]
    nested: Option<Option<Count>>,
    #[schema(value_type = u32, maximum = 2.5)]
    aliased: MaybeCount,
}

#[test]
fn fractional_bounds_range_through_option_layers() {
    let valid = |nested, aliased| Layered { nested, aliased }.validate().is_ok();
    assert!(valid(Some(Some(1)), Some(2)));
    assert!(valid(None, None));
    assert!(valid(Some(None), None));
    assert!(!valid(Some(Some(0)), None), "0 < minimum 0.5");
    assert!(!valid(None, Some(3)), "3 > maximum 2.5");
    assert!(Grouped { count: Some(1) }.validate().is_ok());
    assert!(Grouped { count: Some(0) }.validate().is_err());
}
