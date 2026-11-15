//! Loaded by `schema_type_round16.rs` through `#[path]`: this file is the
//! module `crate::models`, not `crate::schema_type_fixtures::path_models`
//! (M3, round 16: the DTO's field type was rooted at the latter, E0433).

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, vespera::Schema,
)]
pub enum Kind {
    Admin,
    Member,
}

#[derive(Clone, serde::Serialize, serde::Deserialize, vespera::Schema)]
pub struct Member {
    pub id: i32,
    pub kind: Kind,
}

vespera::schema_type!(MemberDto from Member);
