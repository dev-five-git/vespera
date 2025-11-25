use std::collections::HashMap;

use axum::extract::Query;
use serde::Deserialize;
use vespera::Schema;

pub mod health;
pub mod path;
pub mod users;

/// Health check endpoint
#[vespera::route(get)]
pub async fn root_endpoint() -> &'static str {
    "root endpoint"
}

#[vespera::route(get, path = "/hello")]
pub async fn mod_file_endpoint() -> &'static str {
    "mod file endpoint"
}

#[vespera::route(get, path = "/map-query")]
pub async fn mod_file_with_map_query(
    Query(_query): Query<HashMap<String, String>>,
) -> &'static str {
    "mod file endpoint"
}

#[derive(Deserialize, Schema)]
pub struct StructQuery {
    pub name: String,
    pub age: u32,
}

#[vespera::route(get, path = "/struct-query")]
pub async fn mod_file_with_struct_query(Query(_query): Query<StructQuery>) -> &'static str {
    "mod file endpoint"
}
