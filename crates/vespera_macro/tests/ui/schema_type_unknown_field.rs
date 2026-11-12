//! `schema_type!` field lists are checked against the source struct: omitting
//! or picking a field that does not exist (typo) is a compile error listing
//! the available fields, as is a `rename` target that is not an identifier.
use vespera_macro::schema_type;

pub struct User {
    pub id: i32,
    pub name: String,
}

schema_type!(OmitTypo from User, omit = ["nmae"]);

schema_type!(PickTypo from User, pick = ["idd"]);

schema_type!(BadRename from User, rename = [("name", "type")]);

fn main() {}
