use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "config")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(default_value = 0.7)]
    pub temperature: Decimal,
}

vespera::schema_type!(Schema from Model, name = "ConfigSchema");
impl ActiveModelBehavior for ActiveModel {}
