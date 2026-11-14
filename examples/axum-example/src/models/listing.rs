#![allow(dead_code, clippy::struct_field_names)]
//! Relation fields named like the generated `from_model`'s parameters
//! (`model`, `db`), a `from = "Column::.."` column path, and a `belongs_to`
//! keyed by a non-primary-key column (`to = "code"`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "listing")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub model_id: i32,
    pub db_id: Option<i32>,
    pub bakery_code: String,
    #[sea_orm(
        belongs_to,
        relation_enum = "ModelBakery",
        from = "Column::ModelId",
        to = "Id"
    )]
    pub model: HasOne<super::bakery::Entity>,
    #[sea_orm(belongs_to, relation_enum = "DbBakery", from = "db_id", to = "id")]
    pub db: HasOne<super::bakery::Entity>,
    #[sea_orm(
        belongs_to,
        relation_enum = "CodeBakery",
        from = "bakery_code",
        to = "code"
    )]
    pub by_code: HasOne<super::bakery::Entity>,
}

vespera::schema_type!(Schema from Model, name = "ListingSchema");
impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use sea_orm::ConnectionTrait;

    use super::super::cake::tests::bakery_db;
    use super::*;

    /// `model` / `db` relation fields no longer shadow `from_model`'s
    /// parameters, `from = "Column::ModelId"` names the `model_id` field,
    /// and `to = "code"` selects by that column (`find_by_id("B1")` used to
    /// look the code up as a primary key).
    #[tokio::test]
    async fn loads_relations_by_their_referenced_columns() {
        let db = bakery_db().await;
        db.execute_unprepared(
            "INSERT INTO listing (id, model_id, db_id, bakery_code) VALUES (5, 1, 2, 'B1'), (6, 2, NULL, 'B2')",
        )
        .await
        .unwrap();

        let listing = Entity::find_by_id(5).one(&db).await.unwrap().unwrap();
        let loaded = Schema::from_model(listing, &db).await.unwrap();
        assert_eq!(loaded.model.name, "first");
        assert_eq!(loaded.db.map(|b| b.name).as_deref(), Some("second"));
        assert_eq!(loaded.by_code.name, "second");
        assert_eq!(loaded.by_code.flagship.map(|c| c.id), Some(10));

        let listing = Entity::find_by_id(6).one(&db).await.unwrap().unwrap();
        let loaded = Schema::from_model(listing, &db).await.unwrap();
        assert!(loaded.db.is_none());
        assert_eq!(loaded.by_code.name, "first");
    }
}
