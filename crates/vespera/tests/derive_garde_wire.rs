//! `#[derive(Schema)]` validation follows what serde reads from the wire:
//!
//! - a `#[serde(skip)]` / `skip_deserializing` field is never validated
//!   (serde fills its default, so its rule used to reject every request);
//! - a `#[serde(flatten)]` + `#[schema(dive)]` field reports the flattened
//!   keys at the parent's level (`limit`, not `page.limit`);
//! - every `Option` layer is peeled for an integer `range` (a fractional
//!   bound rounds inward), and a type alias keeps its integer bound exact;
//! - an integer bound the `f64` OpenAPI value cannot carry accepts the
//!   advertised value too.
#![cfg(feature = "validation")]

use ::vespera::__validation::garde::Validate;
use ::vespera::Schema;

fn paths(value: &impl Validate<Context = ()>) -> Vec<String> {
    value.validate().map_or_else(
        |report| report.iter().map(|(path, _)| path.to_string()).collect(),
        |()| Vec::new(),
    )
}

#[derive(Schema, serde::Deserialize, Default)]
pub struct Page {
    #[schema(minimum = 1, maximum = 100)]
    pub limit: u32,
}

pub type Count = u64;

#[derive(Schema, serde::Deserialize)]
pub struct Search {
    #[serde(flatten)]
    #[schema(dive)]
    pub page: Page,
    #[serde(skip)]
    #[schema(min_length = 1)]
    pub cache: String,
    #[serde(skip_deserializing)]
    #[schema(minimum = 5)]
    pub hits: u32,
    #[schema(minimum = 0.5)]
    pub ratio: Option<Option<i32>>,
    #[schema(maximum = 3, value_type = u64)]
    pub count: Count,
    #[schema(minimum = 9_007_199_254_740_993_i64)]
    pub big: i64,
}

fn search(json: &str) -> Search {
    serde_json::from_str(json).unwrap()
}

#[test]
fn validation_follows_the_wire() {
    let valid = search(r#"{"limit": 10, "count": 3, "big": 9007199254740993}"#);
    assert!(paths(&valid).is_empty(), "{:?}", paths(&valid));

    // The flattened key reports at the parent's level.
    let flat = search(r#"{"limit": 0, "count": 3, "big": 9007199254740993}"#);
    assert_eq!(paths(&flat), ["limit"]);

    // `minimum = 0.5` on `Option<Option<i32>>` admits 1, not 0.
    let ratio = |r: i32| search(&format!(r#"{{"limit": 1, "ratio": {r}, "count": 0, "big": 9007199254740993}}"#));
    assert_eq!(paths(&ratio(0)), ["ratio"]);
    assert!(paths(&ratio(1)).is_empty());

    // The alias keeps its exact integer bound.
    let count = search(r#"{"limit": 1, "count": 4, "big": 9007199254740993}"#);
    assert_eq!(paths(&count), ["count"]);

    // 2^53 + 1 is advertised as 2^53, which is accepted; below it is not.
    let big = |b: i64| search(&format!(r#"{{"limit": 1, "count": 0, "big": {b}}}"#));
    assert!(paths(&big(9_007_199_254_740_992)).is_empty());
    assert_eq!(paths(&big(9_007_199_254_740_991)), ["big"]);
}
