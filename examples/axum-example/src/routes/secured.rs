//! Security schemes and operation metadata.
//!
//! `vespera!(security_schemes = [...])` in `lib.rs` declares `bearerAuth`
//! (HTTP bearer, JWT) and `apiKey` (`X-API-Key` header) under
//! `components.securitySchemes`; each route here opts in with
//! `#[route(security = [...])]`. `security = []` documents an explicitly
//! unauthenticated operation.
//!
//! The scheme only *documents* auth — enforcing it stays the handler's (or a
//! middleware's) job, shown here with a plain `HeaderMap` check.
//!
//! The routes also show the per-operation metadata attributes:
//! `operation_id`, `summary`, `description`, `response_example`,
//! `deprecated` and `error_status`.

use serde::{Deserialize, Serialize};
use vespera::{
    Schema,
    axum::{
        Json,
        http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    },
};

/// The authenticated caller.
#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct CurrentUser {
    pub id: u32,
    pub name: String,
}

/// Token the demo accepts (`Authorization: Bearer demo-token`).
pub const DEMO_TOKEN: &str = "demo-token";
/// API key the demo accepts (`X-API-Key: demo-key`).
pub const DEMO_API_KEY: &str = "demo-key";

/// Return the caller identified by the bearer token.
#[vespera::route(
    get,
    path = "/me",
    tags = ["secured"],
    security = ["bearerAuth"],
    operation_id = "getCurrentUser",
    summary = "Current user",
    description = "Requires `Authorization: Bearer <token>`; answers 401 otherwise.",
    response_example = r#"{"id": 1, "name": "demo"}"#,
    error_status = [401]
)]
pub async fn current_user(headers: HeaderMap) -> Result<Json<CurrentUser>, StatusCode> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if token != Some(DEMO_TOKEN) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(CurrentUser {
        id: 1,
        name: "demo".to_string(),
    }))
}

/// Legacy report listing, guarded by an API key.
///
/// Marked `deprecated` — Swagger UI renders it struck through.
#[vespera::route(
    get,
    path = "/reports",
    tags = ["secured"],
    security = ["apiKey"],
    deprecated,
    error_status = [401]
)]
pub async fn legacy_reports(headers: HeaderMap) -> Result<Json<Vec<String>>, StatusCode> {
    if headers.get("x-api-key").and_then(|v| v.to_str().ok()) != Some(DEMO_API_KEY) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(vec!["2026-q1".to_string(), "2026-q2".to_string()]))
}

/// Public status — `security = []` marks it explicitly unauthenticated.
#[vespera::route(get, path = "/public", tags = ["secured"], security = [])]
pub async fn public_status() -> &'static str {
    "public"
}
