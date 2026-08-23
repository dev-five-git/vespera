#![allow(dead_code)]

extern crate self as vespera;

use vespera_macro::Schema;

pub trait Schema {}

#[derive(Schema)]
struct Request {
    raw: serde_json::Value,
}

fn main() {}
