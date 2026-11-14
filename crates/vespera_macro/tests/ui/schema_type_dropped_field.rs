//! `schema_type!` naming a field the type does not emit (`omit_default`
//! drops the primary key) and renaming one field twice: both used to be
//! silently ignored.
use vespera_macro::schema_type;

// Never compiled (`sea_orm` is no attribute here); read from the file.
#[cfg(any())]
mod model {
    pub struct Seeded {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
    }
}

schema_type!(RenamedKey from Seeded, omit_default, ignore, rename = [("id", "key")]);

schema_type!(TwiceRenamed from Seeded, ignore, rename = [("name", "a"), ("name", "b")]);

fn main() {}
