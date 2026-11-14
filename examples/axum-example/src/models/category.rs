#![allow(dead_code)]
//! Self-referential `belongs_to` (`self_ref`): `from_model` used to call
//! itself recursively (`E0733`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "category")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub parent_id: Option<i32>,
    #[sea_orm(self_ref, relation_enum = "Parent", from = "ParentId", to = "Id")]
    pub parent: BelongsTo<Option<Entity>>,
}

vespera::schema_type!(Schema from Model, name = "CategorySchema");
impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database};

    use super::*;

    /// The self reference is circular: the parent is an inline type
    /// without relations, so loading stops after one level.
    #[tokio::test]
    async fn self_reference_loads_one_level() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        for sql in [
            "CREATE TABLE category (id INTEGER PRIMARY KEY, name TEXT NOT NULL, parent_id INTEGER)",
            "INSERT INTO category (id, name, parent_id) VALUES (1, 'root', NULL), (2, 'child', 1)",
        ] {
            db.execute_unprepared(sql).await.unwrap();
        }
        let child = Entity::find_by_id(2).one(&db).await.unwrap().unwrap();
        let loaded = Schema::from_model(child, &db).await.unwrap();
        let parent = loaded.parent.expect("parent loaded");
        assert_eq!((parent.name.as_str(), parent.parent_id), ("root", None));
    }
}
