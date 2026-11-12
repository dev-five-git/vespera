//! `schema_type!` parameter errors: `omit` + `pick` together, an unknown
//! parameter, and a repeated parameter.
use vespera_macro::schema_type;

pub struct User {
    pub id: i32,
    pub name: String,
}

schema_type!(BothFilters from User, omit = ["id"], pick = ["name"]);

schema_type!(UnknownParam from User, exclude = ["id"]);

schema_type!(DuplicateParam from User, name = "A", name = "B");

fn main() {}
