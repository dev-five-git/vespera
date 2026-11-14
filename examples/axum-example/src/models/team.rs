#![allow(dead_code)]
//! Two `HasMany` relations to the same entity, told apart by `via_rel`:
//! the generated `from_model` selects each through the target's own FK
//! column (`team_member::Column::TeamId` / `MentorTeamId`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "team")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    #[sea_orm(has_many, relation_enum = "Members", via_rel = "Team")]
    pub members: HasMany<super::team_member::Entity>,
    #[sea_orm(has_many, relation_enum = "Mentees", via_rel = "MentorTeam")]
    pub mentees: HasMany<super::team_member::Entity>,
}

vespera::schema_type!(Schema from Model, name = "TeamSchema");
// `HasMany` relations are only generated when picked explicitly.
vespera::schema_type!(
    TeamWithMembers from Model,
    pick = ["id", "name", "members", "mentees"]
);
impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database};

    use super::super::team_member;
    use super::*;

    /// The generated `from_model` loads each `via_rel` relation through its
    /// own FK column, and each `relation_enum` back-reference by its own
    /// `from` column (previously a non-compiling `Entity::TeamId` path).
    #[tokio::test]
    async fn named_relations_load_through_their_own_fk_columns() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        for sql in [
            "CREATE TABLE team (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE TABLE team_member (id INTEGER PRIMARY KEY, team_id INTEGER NOT NULL, mentor_team_id INTEGER)",
            "INSERT INTO team (id, name) VALUES (1, 'core'), (2, 'infra')",
            "INSERT INTO team_member (id, team_id, mentor_team_id) VALUES (10, 1, 2), (11, 1, NULL), (12, 2, 1)",
        ] {
            db.execute_unprepared(sql).await.unwrap();
        }

        let core = Entity::find_by_id(1).one(&db).await.unwrap().unwrap();
        let loaded = TeamWithMembers::from_model(core, &db).await.unwrap();
        assert_eq!(
            loaded.members.iter().map(|m| m.id).collect::<Vec<_>>(),
            [10, 11]
        );
        assert_eq!(
            loaded.mentees.iter().map(|m| m.id).collect::<Vec<_>>(),
            [12]
        );

        let member = team_member::Entity::find_by_id(10)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        let loaded = team_member::Schema::from_model(member, &db).await.unwrap();
        assert_eq!(loaded.team.name, "core");
        assert_eq!(loaded.mentor_team.map(|t| t.name).as_deref(), Some("infra"));
    }
}
