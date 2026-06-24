#![allow(dead_code)]

extern crate self as vespera;

use vespera_macro::Schema;

pub trait Schema {}

struct MissingSchema;

#[derive(Schema)]
struct Request {
    custom: MissingSchema,
}

fn main() {}
