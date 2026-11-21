//! `schema!(T, omit = [..])` / `pick = [..]` name a raw identifier field by its
//! unraw spelling (`r#type` is the field `type`), and the schema is the one the
//! filtered struct documents (SM1-M1, round 38: the raw spelling matched no
//! field, so `omit` removed nothing and `pick` dropped every field).
#![deny(warnings)]

use serde::{Deserialize, Serialize};
use vespera::Schema;

#[derive(Serialize, Deserialize, Clone, Schema)]
pub struct Message {
    pub id: u32,
    pub r#type: String,
    pub body: String,
}

fn keys(schema: &vespera::schema::Schema) -> Vec<&str> {
    let properties = schema.properties.as_ref().expect("properties");
    properties.keys().map(String::as_str).collect()
}

#[test]
fn omit_of_a_raw_identifier_removes_that_field() {
    let schema = vespera::schema!(Message, omit = ["r#type"]);
    assert_eq!(keys(&schema), ["body", "id"]);
}

#[test]
fn pick_of_a_raw_identifier_keeps_that_field() {
    let schema = vespera::schema!(Message, pick = ["r#type"]);
    assert_eq!(keys(&schema), ["type"]);
}

#[test]
fn unraw_spelling_still_selects_the_raw_identifier_field() {
    let schema = vespera::schema!(Message, omit = ["type"]);
    assert_eq!(keys(&schema), ["body", "id"]);
}
