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
schema_type!(MemoResponse from crate::models::memo::Model, omit = ["updated_at", "user", "memo_comments"]);

schema_type!(MemoResponseRel from crate::models::memo::Model, omit = ["updated_at"]);

schema_type!(MemoResponseComments from crate::models::memo::Model, pick = ["memo_comments"]);

// Test rename_all override: use snake_case instead of default camelCase
schema_type!(MemoSnakeCase from crate::models::memo::Model, pick = ["id", "user_id", "created_at"], rename_all = "snake_case");

#[derive(serde::Serialize, vespera::Schema)]
#[serde(rename_all = "camelCase")]
pub struct UserInMemoDetail {
    pub id: i32,
    pub email: String,
    pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, vespera::Schema)]
#[serde(rename_all = "camelCase")]
pub struct MemoCommentInMemoDetail {
    pub id: i32,
    pub memo_id: i32,
    pub content: String,
}

schema_type!(
    MemoDetailResponse from crate::models::memo::Model,
    relation_adapters = [("user", UserInMemoDetail)],
    add = [("memo_comments": Vec<MemoCommentInMemoDetail>)]
);

// Same-file relation adapter whose OpenAPI component name is overridden via
// `#[schema(name = "...")]`. The generated relation `$ref` must point at that
// schema NAME (`MemoSummaryUser`), not the Rust struct name
// (`UserInMemoSummary`) — otherwise it dangles.
#[derive(serde::Serialize, vespera::Schema)]
#[serde(rename_all = "camelCase")]
#[schema(name = "MemoSummaryUser")]
pub struct UserInMemoSummary {
    pub id: i32,
    pub name: String,
}

schema_type!(
    MemoSummaryResponse from crate::models::memo::Model,
    omit = ["updated_at", "memo_comments"],
    relation_adapters = [("user", UserInMemoSummary)],
    add = [("note": String)]
);

// Adapter DTO that derives `Clone` + `Deserialize` itself (the generated
// relation wrapper must not re-implement them on it), used WITHOUT `add`, so
// `From<Model>` and `from_model` are generated through the wrapper.
#[derive(serde::Serialize, serde::Deserialize, Clone, vespera::Schema)]
#[serde(rename_all = "camelCase")]
pub struct UserInMemoCard {
    pub id: i32,
    pub name: String,
}

schema_type!(
    MemoCardResponse from crate::models::memo::Model,
    omit = ["updated_at", "memo_comments"],
    relation_adapters = [("user", UserInMemoCard)]
);

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
    // Create a default datetime using vespera's chrono re-export
    let now: vespera::chrono::DateTime<vespera::chrono::FixedOffset> =
        vespera::chrono::Utc::now().fixed_offset();
    let model = crate::models::memo::Model {
        id,
        user_id: 1, // Example user ID
        title: "Test Memo".to_string(),
        content: "This is test content".to_string(),
        status: crate::models::memo::MemoStatus::Published,
        created_at: now,
        updated_at: now,
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
    let now: vespera::chrono::DateTime<vespera::chrono::FixedOffset> =
        vespera::chrono::Utc::now().fixed_offset();
    let model = crate::models::memo::Model {
        id,
        user_id: 1, // Example user ID
        title: "Test Memo".to_string(),
        content: "This is test content".to_string(),
        status: crate::models::memo::MemoStatus::Published,
        created_at: now,
        updated_at: now,
    };
    Json(
        MemoResponseRel::from_model(model, app_state.db.as_ref())
            .await
            .unwrap(),
    )
}

#[vespera::route(get, path = "/{id}/detail")]
pub async fn get_memo_detail(Path(id): Path<i32>) -> Json<MemoDetailResponse> {
    let now: vespera::chrono::DateTime<vespera::chrono::FixedOffset> =
        vespera::chrono::Utc::now().fixed_offset();
    let memo = crate::models::memo::Model {
        id,
        user_id: 7,
        title: "Detailed Memo".to_string(),
        content: "Detail content".to_string(),
        status: crate::models::memo::MemoStatus::Published,
        created_at: now,
        updated_at: now,
    };
    let user = Some(crate::models::user::Model {
        id: 7,
        email: "memo@example.com".to_string(),
        name: "Memo User".to_string(),
        created_at: now,
        updated_at: now,
    });
    let memo_comments = vec![MemoCommentInMemoDetail {
        id: 100,
        memo_id: id,
        content: "Looks good".to_string(),
    }];

    Json(MemoDetailResponse {
        id: memo.id,
        user_id: memo.user_id,
        title: memo.title,
        content: memo.content,
        status: memo.status,
        created_at: memo.created_at,
        updated_at: memo.updated_at,
        user: user.into(),
        memo_comments,
    })
}

/// Get a memo summary (same-file relation adapter with a custom schema name)
#[vespera::route(get, path = "/{id}/summary")]
pub async fn get_memo_summary(Path(id): Path<i32>) -> Json<MemoSummaryResponse> {
    let now: vespera::chrono::DateTime<vespera::chrono::FixedOffset> =
        vespera::chrono::Utc::now().fixed_offset();
    let memo = crate::models::memo::Model {
        id,
        user_id: 9,
        title: "Summary Memo".to_string(),
        content: "Summary content".to_string(),
        status: crate::models::memo::MemoStatus::Published,
        created_at: now,
        updated_at: now,
    };
    let user = Some(crate::models::user::Model {
        id: 9,
        email: "summary@example.com".to_string(),
        name: "Summary User".to_string(),
        created_at: now,
        updated_at: now,
    });
    Json(MemoSummaryResponse {
        id: memo.id,
        user_id: memo.user_id,
        title: memo.title,
        content: memo.content,
        status: memo.status,
        created_at: memo.created_at,
        user: user.into(),
        note: "summary".to_string(),
    })
}

/// Get memo response format
#[vespera::route(get, path = "/format")]
pub async fn get_memo_format() -> &'static str {
    "MemoResponse has: id, user_id, title, content, created_at (no updated_at, no user relation)"
}

#[cfg(test)]
mod relation_adapter_tests {
    use sea_orm::{ConnectionTrait, Database, EntityTrait};

    use super::*;

    /// `relation_adapters` without `add`: `from_model` converts the loaded
    /// relation through the wrapper (`From<Option<Model>>`), and the wrapper
    /// clones / deserializes although the DTO derives both itself.
    #[tokio::test]
    async fn relation_adapter_without_add_loads_clones_and_round_trips() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        for sql in [
            "CREATE TABLE user (id INTEGER PRIMARY KEY, email TEXT NOT NULL, name TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "CREATE TABLE memo (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL, title TEXT NOT NULL, content TEXT NOT NULL, status TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            "INSERT INTO user VALUES (1, 'a@b.c', 'Ann', '2024-01-01T00:00:00+00:00', '2024-01-01T00:00:00+00:00')",
            "INSERT INTO memo VALUES (7, 1, 't', 'c', 'draft', '2024-01-01T00:00:00+00:00', '2024-01-01T00:00:00+00:00')",
        ] {
            db.execute_unprepared(sql).await.unwrap();
        }

        let memo = crate::models::memo::Entity::find_by_id(7)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        let card = MemoCardResponse::from_model(memo, &db).await.unwrap();
        let json = serde_json::to_value(card.clone()).unwrap();
        assert_eq!(json["user"], serde_json::json!({ "id": 1, "name": "Ann" }));

        let back: MemoCardResponse = serde_json::from_value(json).unwrap();
        assert_eq!(back.user.0.map(|user| user.name).as_deref(), Some("Ann"));
    }
}

// Serde function paths of a source in another module (`default_theme`,
// `codec`) resolve in that module, not here.
schema_type!(ProfileCard from crate::models::profile::Profile);
schema_type!(ProfilePatch from crate::models::profile::Profile, partial);

/// Apply a profile patch to the default profile
#[vespera::route(patch, path = "/profile")]
pub async fn patch_profile(Json(patch): Json<ProfilePatch>) -> Json<ProfileCard> {
    let base = crate::models::profile::Profile {
        theme: crate::models::profile::default_theme(),
        score: 0,
        bio: String::new(),
    };
    let mut card = ProfileCard::from(base);
    card.theme = patch.theme.unwrap_or(card.theme);
    card.score = patch.score.unwrap_or(card.score);
    card.bio = patch.bio.unwrap_or(card.bio);
    Json(card)
}

#[cfg(test)]
mod serde_path_tests {
    use super::*;

    #[test]
    fn source_module_serde_paths_resolve() {
        let card: ProfileCard = serde_json::from_str(r#"{"score":"7"}"#).unwrap();
        assert_eq!((card.theme.as_str(), card.score), ("light", 7));
        assert_eq!(
            serde_json::to_value(&card).unwrap(),
            serde_json::json!({ "theme": "light", "score": "7" })
        );
        let patch: ProfilePatch = serde_json::from_str(r#"{"score":"3"}"#).unwrap();
        assert_eq!((patch.theme, patch.score), (None, Some(3)));
    }
}
