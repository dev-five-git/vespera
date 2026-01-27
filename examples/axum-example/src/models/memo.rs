use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Memo storage for example-memo-plugin
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "memp_memos")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub title: String,
    pub content: String,
    #[sea_orm(indexed, default_value = "NOW()")]
    pub created_at: DateTimeWithTimeZone,
    #[sea_orm(default_value = "NOW()")]
    pub updated_at: DateTimeWithTimeZone,
}

// Index definitions (SeaORM uses Statement builders externally)
// (unnamed) on [created_at]
impl ActiveModelBehavior for ActiveModel {}
