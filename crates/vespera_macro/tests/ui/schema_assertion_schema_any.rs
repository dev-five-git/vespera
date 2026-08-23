#![allow(dead_code)]

extern crate self as vespera;

use vespera_macro::Schema;

pub trait Schema {}

struct OpaquePayload;

#[derive(Schema)]
struct Request {
    #[schema(any)]
    payload: OpaquePayload,
}

fn main() {}
