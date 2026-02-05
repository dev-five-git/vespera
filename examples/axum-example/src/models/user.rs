use sea_orm::entity::prelude::*;

/// User model
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "user")]
pub struct Model {
    /// User ID
    #[sea_orm(primary_key)]
    pub id: i32,
    /// User email
    #[sea_orm(unique)]
    pub email: String,
    /// User name
    pub name: String,
    /// Created at
    #[sea_orm(default_value = "NOW()")]
    pub created_at: DateTimeWithTimeZone,
    /// Updated at
    #[sea_orm(default_value = "NOW()")]
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(has_many)]
    pub memos: HasMany<super::memo::Entity>,
    #[sea_orm(has_many)]
    pub memo_comments: HasMany<super::memo_comment::Entity>,
}

// Index definitions (SeaORM uses Statement builders externally)
// (unnamed) on [email]
vespera::schema_type!(Schema from Model, name = "UserSchema");
impl ActiveModelBehavior for ActiveModel {}
