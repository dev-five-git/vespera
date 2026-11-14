#![allow(dead_code)]
//! One link of a three-model `belongs_to` cycle (a -> b -> c -> a): the
//! `from_model` chain used to recurse back into itself (`E0733`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "cycle_a")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub label: String,
    pub next_id: Option<i32>,
    #[sea_orm(belongs_to, from = "next_id", to = "id")]
    pub next: BelongsTo<Option<super::cycle_b::Entity>>,
}

vespera::schema_type!(Schema from Model, name = "CycleASchema");
impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database};

    use super::*;

    /// `b` reaches `a` again through `c`, so it is an inline type: the
    /// rows' cycle (a1 -> b1 -> c1 -> a1) is loaded one level deep.
    #[tokio::test]
    async fn cycle_loads_one_level() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        for table in ["cycle_a", "cycle_b", "cycle_c"] {
            db.execute_unprepared(&format!(
                "CREATE TABLE {table} (id INTEGER PRIMARY KEY, label TEXT NOT NULL, next_id INTEGER)"
            ))
            .await
            .unwrap();
            db.execute_unprepared(&format!(
                "INSERT INTO {table} (id, label, next_id) VALUES (1, '{table}', 1)"
            ))
            .await
            .unwrap();
        }
        let a = Entity::find_by_id(1).one(&db).await.unwrap().unwrap();
        let loaded = Schema::from_model(a, &db).await.unwrap();
        let b = loaded.next.expect("next loaded");
        assert_eq!((b.label.as_str(), b.next_id), ("cycle_b", Some(1)));
    }
}
