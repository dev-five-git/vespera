#![allow(dead_code)]
//! SeaORM 2.0 optional `belongs_to`: `BelongsTo<Option<Entity>>` (the
//! relation path used to be built from `Option` itself).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "cake")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub bakery_id: Option<i32>,
    pub flagship_of_id: Option<i32>,
    #[sea_orm(belongs_to, relation_enum = "Bakery", from = "bakery_id", to = "id")]
    pub bakery: BelongsTo<Option<super::bakery::Entity>>,
    #[sea_orm(
        belongs_to,
        relation_enum = "FlagshipOf",
        from = "flagship_of_id",
        to = "id"
    )]
    pub flagship_of: BelongsTo<Option<super::bakery::Entity>>,
}

vespera::schema_type!(Schema from Model, name = "CakeSchema");
impl ActiveModelBehavior for ActiveModel {}

/// One adapter DTO for both bakery relations (and for two `schema_type!`
/// calls): the generated conversions are wrapper-local, so it is never
/// implemented twice (`E0119`). The container `default` and the `#[cfg]`
/// of the (compiled-out) field carry over to the wrapper's conversions and
/// deserialize proxy.
#[derive(serde::Serialize, serde::Deserialize, Default, vespera::Schema)]
#[serde(default, rename_all = "camelCase")]
pub struct BakeryInCake {
    pub id: i32,
    pub name: String,
    #[cfg(any())]
    pub code: String,
}

vespera::schema_type!(
    CakeWithBakeries from Model,
    relation_adapters = [("bakery", BakeryInCake), ("flagship_of", BakeryInCake)]
);
vespera::schema_type!(
    CakeBakeryOnly from Model,
    pick = ["id", "bakery"],
    relation_adapters = [("bakery", BakeryInCake)]
);

// Two relations to `bakery`, so SeaORM derives no `Related` impl; the
// bakery's `has_one` (`Flagship`) needs one.
impl Related<super::bakery::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::FlagshipOf.def()
    }
}

#[cfg(test)]
pub mod tests {
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection};

    use super::super::bakery;
    use super::*;

    /// An in-memory database with the `bakery` / `cake` / `listing` tables.
    pub async fn bakery_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        for sql in [
            "CREATE TABLE bakery (id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE, name TEXT NOT NULL)",
            "CREATE TABLE cake (id INTEGER PRIMARY KEY, name TEXT NOT NULL, bakery_id INTEGER, flagship_of_id INTEGER)",
            "CREATE TABLE listing (id INTEGER PRIMARY KEY, model_id INTEGER NOT NULL, db_id INTEGER, bakery_code TEXT NOT NULL)",
            "INSERT INTO bakery (id, code, name) VALUES (1, 'B2', 'first'), (2, 'B1', 'second')",
            "INSERT INTO cake (id, name, bakery_id, flagship_of_id) VALUES (10, 'sponge', 1, 2), (11, 'loose', NULL, NULL)",
        ] {
            db.execute_unprepared(sql).await.unwrap();
        }
        db
    }

    /// `BelongsTo<Option<Entity>>` targets the entity (it used to build
    /// `crate::models::cake::Option`) and is optional; `HasOne` +
    /// `via_rel` selects through the target's relation of that name.
    #[tokio::test]
    async fn optional_belongs_to_and_has_one_via_rel_load() {
        let db = bakery_db().await;
        let sponge = Entity::find_by_id(10).one(&db).await.unwrap().unwrap();
        let loaded = Schema::from_model(sponge, &db).await.unwrap();
        assert_eq!(loaded.bakery.map(|b| b.name).as_deref(), Some("first"));
        assert_eq!(loaded.flagship_of.map(|b| b.code).as_deref(), Some("B1"));

        let loose = Entity::find_by_id(11).one(&db).await.unwrap().unwrap();
        let loaded = Schema::from_model(loose, &db).await.unwrap();
        assert!(loaded.bakery.is_none() && loaded.flagship_of.is_none());

        let second = bakery::Entity::find_by_id(2)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        let loaded = bakery::Schema::from_model(second, &db).await.unwrap();
        assert_eq!(loaded.flagship.map(|c| c.name).as_deref(), Some("sponge"));
    }

    /// The same adapter DTO serves two fields and two `schema_type!` calls;
    /// a missing member deserializes from the DTO's `Default`.
    #[tokio::test]
    async fn shared_adapter_loads_and_defaults_missing_members() {
        let db = bakery_db().await;
        let sponge = Entity::find_by_id(10).one(&db).await.unwrap().unwrap();
        let both = CakeWithBakeries::from_model(sponge.clone(), &db)
            .await
            .unwrap();
        assert_eq!(both.bakery.0.map(|b| b.name).as_deref(), Some("first"));
        assert_eq!(
            both.flagship_of.0.map(|b| b.name).as_deref(),
            Some("second")
        );
        let one = CakeBakeryOnly::from_model(sponge, &db).await.unwrap();
        assert_eq!(one.bakery.0.map(|b| b.id), Some(1));

        let parsed: CakeBakeryOnly =
            serde_json::from_str(r#"{"id":3,"bakery":{"name":"x"}}"#).unwrap();
        let bakery = parsed.bakery.0.expect("bakery present");
        assert_eq!((bakery.id, bakery.name.as_str()), (0, "x"));
    }
}
