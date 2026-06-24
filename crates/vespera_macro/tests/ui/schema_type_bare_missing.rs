use vespera_macro::schema_type;

pub struct LocalModel {
    pub id: i32,
}

schema_type!(Schema from NonexistentInThisFile, name = "MissingSchema");

fn main() {}
