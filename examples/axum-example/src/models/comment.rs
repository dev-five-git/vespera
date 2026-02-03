use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "comment")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(indexed)]
    pub user_id: i32,
    #[sea_orm(indexed)]
    pub memo_id: i32,
    pub content: String,
    #[sea_orm(default_value = "NOW()")]
    pub created_at: DateTimeWithTimeZone,
    #[sea_orm(default_value = "NOW()")]
    pub updated_at: DateTimeWithTimeZone,
    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: HasOne<super::user::Entity>,
    #[sea_orm(belongs_to, from = "memo_id", to = "id")]
    pub memo: HasOne<super::memo::Entity>,
}

// Index definitions (SeaORM uses Statement builders externally)
// (unnamed) on [user_id]
// (unnamed) on [memo_id]
vespera::schema_type!(Schema from Model, name = "CommentSchema");
impl ActiveModelBehavior for ActiveModel {}
