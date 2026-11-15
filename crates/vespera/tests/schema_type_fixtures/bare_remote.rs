//! A `#[derive(Schema)]` `User` of another file, registered before the
//! `schema_type!` in `schema_type_bare_local.rs` names its own `User`.

use serde::{Deserialize, Serialize};
use vespera::Schema;

#[derive(Schema, Serialize, Deserialize, Clone)]
pub struct User {
    pub id: u32,
    pub email: String,
}
