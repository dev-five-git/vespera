use std::collections::BTreeSet;

use uuid::Uuid;
use vespera::{axum::Json, schema_type};

use crate::models::uuid_item;

schema_type!(UuidItem from crate::models::uuid_item::Model, omit = ["created_at"], add = [("tags": BTreeSet<String>)]);
schema_type!(CreateUuidItemRequest from crate::models::uuid_item::Model, omit_default, add = [("tags": BTreeSet<String>)]);
// A source path written through this module's import (`uuid_item::Model`)
// resolves through it; it used to be read as `crate::uuid_item`.
schema_type!(UuidItemName from uuid_item::Model, pick = ["id", "name"]);
// A cross-file `DateTimeLocal` field with a `NOW()` default: the value
// depends on the server's offset, so no OpenAPI default is advertised, yet
// the DTO compiles and serde fills an omitted `created_at`.
schema_type!(ClockDto from crate::models::clock::Model);

/// Echo a clock, filling an omitted `createdAt` with the epoch
#[vespera::route(post, path = "/clock", tags = ["uuid_items"])]
pub async fn echo_clock(Json(clock): Json<ClockDto>) -> Json<ClockDto> {
    Json(clock)
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
        tags: BTreeSet::new(),
    }])
}

/// Create a new UUID item
#[vespera::route(post, tags = ["uuid_items"])]
pub async fn create_uuid_item(Json(req): Json<CreateUuidItemRequest>) -> Json<UuidItem> {
    Json(UuidItem {
        id: Uuid::new_v4(),
        name: req.name,
        external_ref: req.external_ref,
        tags: req.tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_through_import_converts() {
        let id = Uuid::nil();
        let named = UuidItemName::from(uuid_item::Model {
            id,
            name: "n".into(),
            external_ref: None,
            created_at: Default::default(),
        });
        assert_eq!((named.id, named.name.as_str()), (id, "n"));
    }
}
