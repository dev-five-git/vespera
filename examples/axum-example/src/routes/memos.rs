//! Test schema_type! with models from other files
//!
//! This demonstrates that schema_type! can reference structs from other files
//! using module paths like `crate::models::memo::Model`. The macro will:
//! 1. Parse the module path
//! 2. Find the corresponding file (src/models/memo.rs)
//! 3. Extract the struct definition
//! 4. Generate the new type + From impl for easy conversion

use std::sync::Arc;

// Import types used by the source model that we want to include in generated structs
use sea_orm::entity::prelude::DateTimeWithTimeZone;
use vespera::{
    axum::{
        Json,
        extract::{Path, State},
    },
    schema_type,
};

use crate::AppState;

// ============================================================================
// schema_type! generates request/response types from models in OTHER FILES
// Also generates From<SourceType> impl when `add` is not used
// ============================================================================

// Create request type: only title and content (no id, timestamps)
// Has From impl: crate::models::memo::Model -> CreateMemoRequest
schema_type!(CreateMemoRequest from crate::models::memo::Model, pick = ["title", "content"]);

// Update request: title/content + manually added id field
// NO From impl (because `add` is used - can't auto-populate added fields)
schema_type!(UpdateMemoRequest from crate::models::memo::Model, pick = ["title", "content"], add = [("id": i32)]);

// Response type: all fields except updated_at and relations
// Has From impl since we omit all relation fields
schema_type!(MemoResponse from crate::models::memo::Model, omit = ["updated_at", "user", "comments"]);

schema_type!(MemoResponseRel from crate::models::memo::Model, omit = ["updated_at"]);

schema_type!(MemoResponseComments from crate::models::memo::Model, pick = ["comments"]);

// Test rename_all override: use snake_case instead of default camelCase
schema_type!(MemoSnakeCase from crate::models::memo::Model, pick = ["id", "user_id", "created_at"], rename_all = "snake_case");

/// Create a new memo
#[vespera::route(post)]
pub async fn create_memo(Json(req): Json<CreateMemoRequest>) -> Json<CreateMemoRequest> {
    // Echo back the request to verify it works
    Json(CreateMemoRequest {
        title: req.title,
        content: req.content,
    })
}

/// Update a memo
#[vespera::route(put)]
pub async fn update_memo(Json(req): Json<UpdateMemoRequest>) -> Json<UpdateMemoRequest> {
    // Echo back - demonstrates `add` parameter with sea-orm model
    Json(UpdateMemoRequest {
        id: req.id,
        title: req.title,
        content: req.content,
    })
}

/// Get memo by id
#[vespera::route(get, path = "/{id}")]
pub async fn get_memo(Path(id): Path<i32>) -> Json<MemoResponse> {
    // In real app, this would be a DB query returning Model
    // schema_type! generates From<Model> for MemoResponse, so .into() works
    let model = crate::models::memo::Model {
        id,
        user_id: 1, // Example user ID
        title: "Test Memo".to_string(),
        content: "This is test content".to_string(),
        status: crate::models::memo::MemoStatus::Published,
        created_at: DateTimeWithTimeZone::default(),
        updated_at: DateTimeWithTimeZone::default(),
    };
    Json(model.into())
}

#[vespera::route(get, path = "/{id}/rel")]
pub async fn get_memo_rel(
    Path(id): Path<i32>,
    State(app_state): State<Arc<AppState>>,
) -> Json<MemoResponseRel> {
    // In real app, this would be a DB query returning Model
    // schema_type! generates From<Model> for MemoResponse, so .into() works
    let model = crate::models::memo::Model {
        id,
        user_id: 1, // Example user ID
        title: "Test Memo".to_string(),
        content: "This is test content".to_string(),
        status: crate::models::memo::MemoStatus::Published,
        created_at: DateTimeWithTimeZone::default(),
        updated_at: DateTimeWithTimeZone::default(),
    };
    Json(
        MemoResponseRel::from_model(model, app_state.db.as_ref())
            .await
            .unwrap(),
    )
}

/// Get memo response format
#[vespera::route(get, path = "/format")]
pub async fn get_memo_format() -> &'static str {
    "MemoResponse has: id, user_id, title, content, created_at (no updated_at, no user relation)"
}
