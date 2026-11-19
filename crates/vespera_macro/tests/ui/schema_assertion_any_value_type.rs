#![allow(dead_code)]

extern crate self as vespera;

use vespera_macro::Schema;

pub trait Schema {}

struct OpaquePayload;

// `any` opts a field out of the leaf-type assertion only when no `value_type`
// documents it: this field is documented through `OpaquePayload`, which does
// not derive `Schema`, so the derive must still refuse it.
#[derive(Schema)]
struct Request {
    #[schema(any, value_type = OpaquePayload)]
    payload: u8,
}

fn main() {}
