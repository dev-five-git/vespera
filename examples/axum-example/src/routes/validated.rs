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
use vespera::axum::{Json, extract::Path};
use vespera::{Schema, Validated, ValidatedWith};

use crate::TagPolicy;

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

/// Replace a validated user.
///
/// A `Path` extractor may come BEFORE the validated body — the `422`
/// response is still documented.
#[vespera::route(put, path = "/users/{id}", tags = ["validated"])]
pub async fn update_validated_user(
    Path(id): Path<u64>,
    Validated(Json(req)): Validated<Json<ValidatedUserRequest>>,
) -> Json<ValidatedUserRequest> {
    let _ = id;
    Json(req)
}

/// Tagged post, validated against the runtime tag policy.
//
// Context-aware rules are garde's own: derive `garde::Validate` with
// `#[garde(context(..))]` and keep `#[schema(...)]` rules off the type (a
// `#[schema]` rule would make `Schema` emit a second, context-free impl).
#[derive(Debug, Deserialize, Serialize, Schema, garde::Validate)]
#[garde(context(TagPolicy as policy))]
pub struct TaggedPost {
    /// Post title.
    #[garde(length(min = 1))]
    pub title: String,

    /// Tags — each must start with the policy's `required_prefix`.
    #[garde(custom(|tags: &Vec<String>, policy: &TagPolicy| {
        if tags.iter().all(|tag| tag.starts_with(&policy.required_prefix)) {
            Ok(())
        } else {
            Err(garde::Error::new(format!(
                "every tag must start with `{}`",
                policy.required_prefix
            )))
        }
    }))]
    pub tags: Vec<String>,
}

/// Create a tagged post.
///
/// `ValidatedWith<C, T>` validates with a context `C` borrowed from the
/// router state (`impl ValidationContext<TagPolicy> for Arc<AppState>`); a
/// rule violation answers the same `422` envelope.
#[vespera::route(post, path = "/tags", tags = ["validated"])]
pub async fn create_tagged_post(
    post: ValidatedWith<TagPolicy, Json<TaggedPost>>,
) -> Json<TaggedPost> {
    post.into_inner()
}

/// Profile validated by garde's own derive — `#[derive(Schema)]` documents
/// every `#[garde(...)]` rule with a JSON Schema equivalent, so the
/// constraints in `openapi.json` match the `422` this route answers.
#[derive(Debug, Deserialize, Serialize, Schema, garde::Validate)]
pub struct GardeProfile {
    /// 2–20 characters (`chars` mode → `minLength` / `maxLength`).
    #[garde(length(chars, min = 2, max = 20))]
    pub display_name: String,

    /// `email` → `format: email`.
    #[garde(email)]
    pub email: String,

    /// Optional homepage — `url` → `format: uri` on the non-null value.
    #[garde(url)]
    pub homepage: Option<String>,

    /// `range` → `minimum` / `maximum` (inclusive).
    #[garde(range(min = 18, max = 120))]
    pub age: u8,

    /// 1–3 hashtags, each a `#`-prefixed ASCII word of at most 16 chars
    /// (`inner(...)` → `items`).
    #[garde(
        length(min = 1, max = 3),
        inner(length(chars, max = 16), prefix("#"), ascii)
    )]
    pub hashtags: Vec<String>,

    /// `ip` → `anyOf: [{format: ipv4}, {format: ipv6}]`.
    #[garde(ip)]
    pub last_ip: String,

    /// ISO 3166-1 alpha-2 code (`pattern`).
    #[garde(pattern(r"^[A-Z]{2}$"))]
    pub country: String,

    /// Avatar file name ending in `.png` (`suffix` → escaped `pattern`).
    #[garde(suffix(".png"))]
    pub avatar: String,

    /// Unicode-aware `alphanumeric` has no portable regex — enforced at
    /// runtime, not documented.
    #[garde(alphanumeric)]
    pub nickname: String,
}

/// Create a profile validated by `#[derive(garde::Validate)]`.
#[vespera::route(post, path = "/profiles", tags = ["validated"])]
pub async fn create_garde_profile(
    Validated(Json(profile)): Validated<Json<GardeProfile>>,
) -> Json<GardeProfile> {
    Json(profile)
}
