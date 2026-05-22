//! Demonstration of the `#[derive(Schema)]` validation feature.
//!
//! Field-level `#[schema(min_length=..., max_length=..., pattern=...,
//! format=..., minimum=..., maximum=..., min_items=..., max_items=...)]`
//! attributes drive **both** the OpenAPI metadata for `openapi.json`
//! **and** the runtime `garde::Validate` impl wired up by the
//! `vespera::Validated` extractor.
//!
//! Send a bad payload to this route to see the `422 Unprocessable
//! Entity + { "errors": [...] }` response shape; a good payload
//! returns `200 OK` with an echo of the validated request.
//!
//! NOTE: the type is named `ValidatedUserRequest` — *not*
//! `CreateUserRequest` — to avoid clashing with the existing
//! `schema_type!(CreateUserRequest from User, ...)` in
//! `routes/users.rs`.  Two derives with the same struct identifier
//! both register into the global `SCHEMA_STORAGE` map and the later
//! one silently overrides the earlier one in the emitted
//! `openapi.json`.

use serde::{Deserialize, Serialize};
use vespera::axum::Json;
use vespera::{Schema, Validated};

/// Validated request body for `POST /validated/users`.
#[derive(Debug, Deserialize, Serialize, Schema)]
pub struct ValidatedUserRequest {
    /// User-chosen handle.
    #[schema(min_length = 3, max_length = 32, pattern = "^[a-z0-9_]+$")]
    pub username: String,

    /// Primary contact email — validated at the format level.
    #[schema(format = "email")]
    pub email: String,

    /// Display age (0–150).
    #[schema(minimum = 0, maximum = 150)]
    pub age: u32,

    /// Arbitrary tag list, 1–5 items.
    #[schema(min_items = 1, max_items = 5)]
    pub tags: Vec<String>,
}

/// Echo back the validated input.  If the request body fails
/// validation, this handler never runs — the `Validated` extractor
/// returns a `422` before it is reached.
#[vespera::route(post, path = "/users", tags = ["validated"])]
pub async fn create_validated_user(
    Validated(Json(req)): Validated<Json<ValidatedUserRequest>>,
) -> Json<ValidatedUserRequest> {
    Json(req)
}

