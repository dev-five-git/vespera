//! Handlers that take an extractor's rejection themselves
//! (`Result<Extractor, Rejection>`): the request is documented exactly like
//! the bare extractor — a required body / the query parameters.

use serde::{Deserialize, Serialize};
use vespera::axum::{
    Json,
    extract::{
        Query,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
};
use vespera::{Schema, route};

#[derive(Deserialize, Serialize, Schema)]
pub struct RejectionInput {
    pub name: String,
}

#[derive(Deserialize, Schema)]
pub struct RejectionFilter {
    pub q: String,
    pub limit: Option<u32>,
}

/// Echo the name, answering a malformed body with a plain-text `400`.
#[route(post, error_status = [400])]
pub async fn create_lenient(
    body: Result<Json<RejectionInput>, JsonRejection>,
) -> Result<Json<RejectionInput>, (StatusCode, String)> {
    body.map_err(|rejection| (StatusCode::BAD_REQUEST, rejection.body_text()))
}

/// Echo the query, or name the rejection when it does not parse.
#[route(get)]
pub async fn search_lenient(
    query: std::result::Result<Query<RejectionFilter>, QueryRejection>,
) -> String {
    match query {
        Ok(Query(filter)) => format!("{}:{}", filter.q, filter.limit.unwrap_or(0)),
        Err(_) => "invalid query".to_string(),
    }
}
