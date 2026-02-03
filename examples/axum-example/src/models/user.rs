use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub email: String,
    pub name: String,
    #[sea_orm(default_value = "NOW()")]
    pub created_at: DateTimeWithTimeZone,
    #[sea_orm(default_value = "NOW()")]
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(has_many)]
    pub comments: HasMany<super::comment::Entity>,
    #[sea_orm(has_many)]
    pub memos: HasMany<super::memo::Entity>,
}

// Schema WITH memos relation - circular refs are auto-handled
// When embedded in MemoSchema.user, the memos field will be defaulted to vec![]
// Custom OpenAPI name: "UserSchema"
vespera::schema_type!(Schema from Model, name = "UserSchema");

// Index definitions (SeaORM uses Statement builders externally)
// (unnamed) on [email]
impl ActiveModelBehavior for ActiveModel {}
