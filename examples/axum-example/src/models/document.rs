use sea_orm::{JsonValue, entity::prelude::*};

/// A free-form JSON document. SeaORM's `Json` / `JsonValue` are aliases of
/// `serde_json::Value`, so the columns can hold objects, arrays AND scalars —
/// documented as any value (`{}`), not `{type: object}`.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "document")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Document body.
    pub body: Json,
    /// Optional metadata.
    pub meta: Option<JsonValue>,
}

impl ActiveModelBehavior for ActiveModel {}
