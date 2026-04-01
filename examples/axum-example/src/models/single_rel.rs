use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "single_rel")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub username: String,
    #[sea_orm(belongs_to, from = "username", to = "username")]
    pub single: HasOne<crate::models::single::Entity>,
}

vespera::schema_type!(Schema from Model, name = "SingleRelSchema");
impl ActiveModelBehavior for ActiveModel {}
