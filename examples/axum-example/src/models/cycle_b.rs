#![allow(dead_code)]
//! One link of a three-model `belongs_to` cycle (a -> b -> c -> a).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "cycle_b")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub label: String,
    pub next_id: Option<i32>,
    #[sea_orm(belongs_to, from = "next_id", to = "id")]
    pub next: BelongsTo<Option<super::cycle_c::Entity>>,
}

vespera::schema_type!(Schema from Model, name = "CycleBSchema");
impl ActiveModelBehavior for ActiveModel {}
