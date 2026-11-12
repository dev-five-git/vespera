//! Compile-fail: a `Validated` / `ValidatedWith` payload must implement
//! `garde::Validate`. `#[derive(Schema)]` only generates it for a named-field
//! struct with at least one runtime `#[schema(...)]` rule, so a rule-less
//! struct or an enum is reported with vespera's own message at the argument
//! (instead of an opaque `Handler` trait-bound error at the router).

use vespera::{Schema, Validated, axum::Json};

#[derive(serde::Deserialize, Schema)]
pub struct NoRules {
    pub name: String,
}

#[derive(serde::Deserialize, Schema)]
pub enum Kind {
    A,
    B,
}

#[vespera::route(post)]
pub async fn create(Validated(Json(body)): Validated<Json<NoRules>>) -> String {
    body.name
}

#[vespera::route(get)]
pub async fn by_kind(
    Validated(vespera::axum::extract::Query(_kind)): Validated<vespera::axum::extract::Query<Kind>>,
) -> &'static str {
    "ok"
}

fn main() {}
