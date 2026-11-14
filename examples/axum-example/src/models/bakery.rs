#![allow(dead_code)]
//! Target of the SeaORM 2.0 relation shapes `cake` / `listing` exercise:
//! an optional `BelongsTo<Option<Entity>>`, `HasOne` + `via_rel`, a key
//! that is not the primary key (`code`), and relation fields named like the
//! generated `from_model`'s own parameters (`model`, `db`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "bakery")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub code: String,
    pub name: String,
    /// `HasOne` selected through the target's `FlagshipOf` relation.
    #[sea_orm(has_one, relation_enum = "Flagship", via_rel = "FlagshipOf")]
    pub flagship: HasOne<super::cake::Entity>,
    #[sea_orm(has_many, relation_enum = "Cakes", via_rel = "Bakery")]
    pub cakes: HasMany<super::cake::Entity>,
}

vespera::schema_type!(Schema from Model, name = "BakerySchema");
impl ActiveModelBehavior for ActiveModel {}
