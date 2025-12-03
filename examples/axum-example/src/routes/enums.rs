use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use vespera::{Schema, axum::Json};

use crate::TestStruct;

#[derive(Serialize, Deserialize, Schema)]
pub enum Enum {
    A,
    B,
    C,
}

#[vespera::route(get)]
pub async fn enum_endpoint() -> Json<Enum> {
    Json(Enum::A)
}

#[derive(Serialize, Deserialize, Schema)]
pub enum Enum2 {
    A(String),
    B { name: String, age: i32 },
    C(i32),
    D(bool),
    E(Vec<String>),
    F(String, i32),
    G(HashMap<String, String>),
    H(BTreeMap<String, String>),
    I(TestStruct),
    J(Vec<TestStruct>),
    K(TestStruct, TestStruct),
    L(Option<String>),
    M(Vec<Option<String>>),
    N(HashMap<String, Option<String>>),
}

#[vespera::route(get, path = "/enum2")]
pub async fn enum_endpoint2() -> Json<Enum2> {
    Json(Enum2::A("a".to_string()))
}
