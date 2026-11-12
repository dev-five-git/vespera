//! `schema!` accepts either `omit` or `pick`, never both, and rejects
//! unknown parameters.
#![allow(dead_code)]

extern crate self as vespera;

use vespera_macro::{Schema, schema};

pub trait Schema {}

#[derive(Schema)]
pub struct User {
    pub id: i32,
    pub name: String,
}

fn both() {
    let _ = schema!(User, omit = ["id"], pick = ["name"]);
}

fn unknown() {
    let _ = schema!(User, only = ["id"]);
}

fn main() {}
