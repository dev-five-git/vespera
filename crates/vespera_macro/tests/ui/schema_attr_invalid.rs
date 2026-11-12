//! Malformed values of known `#[schema(...)]` keys are compile errors instead
//! of being silently dropped from the generated OpenAPI.
#![allow(dead_code)]

extern crate self as vespera;

use vespera_macro::Schema;

pub trait Schema {}

#[derive(Schema)]
struct NonNumericBound {
    #[schema(minimum = "zero")]
    count: i32,
}

#[derive(Schema)]
struct UnsupportedExample {
    #[schema(example = b'x')]
    tag: String,
}

fn main() {}
