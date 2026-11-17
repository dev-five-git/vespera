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

/// Echo the name, answering a malformed body with a plain-text error whose
/// status is axum's own for the rejection (`400` syntax, `415` no JSON
/// content type, `422` wrong shape).
#[route(post, error_status = [400, 415, 422])]
pub async fn create_lenient(
    body: Result<Json<RejectionInput>, JsonRejection>,
) -> Result<Json<RejectionInput>, (StatusCode, String)> {
    body.map_err(|rejection| (rejection.status(), rejection.body_text()))
}

/// Echo the query, or answer `invalid query` with `400` when it does not parse.
#[route(get, error_status = [400])]
pub async fn search_lenient(
    query: std::result::Result<Query<RejectionFilter>, QueryRejection>,
) -> Result<String, (StatusCode, String)> {
    match query {
        Ok(Query(filter)) => Ok(format!("{}:{}", filter.q, filter.limit.unwrap_or(0))),
        Err(_) => Err((StatusCode::BAD_REQUEST, "invalid query".to_string())),
    }
}
