//! `schema_type!` field references that conflict with the rest of the
//! invocation point at the offending name: `rename` / `partial` /
//! `relation_adapters` of a field the type does not include, and adapter
//! entries that name no field, repeat one, or name a non-relation field.
use vespera_macro::schema_type;

pub struct User {
    pub id: i32,
    pub name: String,
}

pub struct UserCard {
    pub id: i32,
}

schema_type!(RenameOmitted from User, omit = ["name"], rename = [("name", "title")]);

schema_type!(PartialUnpicked from User, pick = ["id"], partial = ["name"]);

schema_type!(AdapterUnknown from User, relation_adapters = [("owner", UserCard)]);

schema_type!(AdapterTwice from User, relation_adapters = [("id", UserCard), ("id", UserCard)]);

schema_type!(AdapterOmitted from User, omit = ["name"], relation_adapters = [("name", UserCard)]);

schema_type!(AdapterPlain from User, relation_adapters = [("name", UserCard)]);

schema_type!(AddInvalid from User, add = [("type": i32)]);

fn main() {}
