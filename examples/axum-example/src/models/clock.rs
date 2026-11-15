#![allow(dead_code)]

use sea_orm::entity::prelude::*;

/// Clock model: a server-local `NOW()` timestamp (`DateTimeLocal`).
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "clock")]
pub struct Model {
    /// Clock ID
    #[sea_orm(primary_key)]
    pub id: i32,
    /// Created at (server-local time)
    #[sea_orm(default_value = "NOW()")]
    pub created_at: DateTimeLocal,
}

impl ActiveModelBehavior for ActiveModel {}
