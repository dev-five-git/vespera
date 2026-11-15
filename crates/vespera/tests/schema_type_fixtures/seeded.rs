//! Source-only fixture for `schema_type_field_attrs.rs`: `schema_type!`
//! reads this file by module path (`crate::schema_type_fixtures::seeded`),
//! but cargo never compiles it (it is not a `tests/*.rs` target and no
//! `mod` declares it), so the `#[sea_orm(..)]` attributes need no derive.

pub struct Seeded {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(default_value = "5")]
    pub count: i32,
    /// serde's own default (`0`) wins over the SeaORM literal.
    #[sea_orm(default_value = "42")]
    #[serde(default)]
    pub bonus: i32,
}
