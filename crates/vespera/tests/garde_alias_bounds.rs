//! A fractional `minimum` / `maximum` on a field whose type the macro cannot
//! see through (a type alias) rounds inward for an integer target — a
//! truncating `0.5 as u32` accepted `0` while the document says
//! `minimum: 0.5` — and keeps the value for a float target.

#![cfg(feature = "validation")]

use ::vespera::__validation::garde::Validate;
use ::vespera::Schema;

type Count = u32;
type Ratio = f64;

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct Bounded {
    #[schema(minimum = 0.5, maximum = 2.5)]
    count: Count,
    #[schema(minimum = 0.5, maximum = 2.5)]
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
