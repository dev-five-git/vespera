#![allow(dead_code)]
//! Two `belongs_to` relations to the same entity, told apart by
//! `relation_enum`: loaded by their own FK columns (`find_by_id`).
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "team_member")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub team_id: i32,
    pub mentor_team_id: Option<i32>,
    #[sea_orm(belongs_to, relation_enum = "Team", from = "team_id", to = "id")]
    pub team: HasOne<super::team::Entity>,
    #[sea_orm(
        belongs_to,
        relation_enum = "MentorTeam",
        from = "mentor_team_id",
        to = "id"
    )]
    pub mentor_team: HasOne<super::team::Entity>,
}

vespera::schema_type!(Schema from Model, name = "TeamMemberSchema");
impl ActiveModelBehavior for ActiveModel {}
