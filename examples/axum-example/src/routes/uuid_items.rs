use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vespera::Schema;
use vespera::axum::Json;

#[derive(Serialize, Deserialize, Schema)]
pub struct UuidItem {
    pub id: Uuid,
    pub name: String,
    pub external_ref: Option<Uuid>,
}

#[derive(Deserialize, Schema)]
pub struct CreateUuidItemRequest {
    pub name: String,
    pub external_ref: Option<Uuid>,
}

/// List all UUID items
#[vespera::route(get, tags = ["uuid_items"])]
pub async fn list_uuid_items() -> Json<Vec<UuidItem>> {
    let _ = crate::models::uuid_item::Model {
        id: Uuid::new_v4(),
        name: "example".to_string(),
        external_ref: Some(Uuid::new_v4()),
        created_at: Default::default(),
    };
    Json(vec![UuidItem {
        id: Uuid::new_v4(),
        name: "example".to_string(),
        external_ref: Some(Uuid::new_v4()),
    }])
}

/// Create a new UUID item
#[vespera::route(post, tags = ["uuid_items"])]
pub async fn create_uuid_item(Json(req): Json<CreateUuidItemRequest>) -> Json<UuidItem> {
    Json(UuidItem {
        id: Uuid::new_v4(),
        name: req.name,
        external_ref: req.external_ref,
    })
}
