use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "single")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub username: String,
    #[sea_orm(has_one)]
    pub single_rel: HasOne<crate::models::single_rel::Entity>,
}

vespera::schema_type!(Schema from Model, name = "SingleSchema");
impl ActiveModelBehavior for ActiveModel {}
