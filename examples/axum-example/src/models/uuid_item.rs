use sea_orm::entity::prelude::*;

/// UUID item model for testing UUID format in OpenAPI
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "uuid_item")]
pub struct Model {
    /// Item ID
    #[sea_orm(primary_key, default_value = "gen_random_uuid()")]
    pub id: Uuid,
    /// Item name
    pub name: String,
    /// External reference UUID
    pub external_ref: Option<Uuid>,
    /// Created at
    #[sea_orm(default_value = "NOW()")]
    pub created_at: DateTimeWithTimeZone,
}

vespera::schema_type!(Schema from Model, name = "UuidItemSchema");
impl ActiveModelBehavior for ActiveModel {}
