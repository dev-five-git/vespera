//! Two inline `mod`s of one file each define a `#[derive(Schema)]` struct of
//! the same name whose `#[serde(default = "..")]` names a function of its own
//! module: the derive picks its module by the struct's line, which needs the
//! file re-parsed with real span positions (inside the compiler every
//! re-parsed token points at the call site, so no candidate matched and the
//! default was "not statically determinable" — a compile error).
#![deny(warnings)]

mod first {
    use serde::{Deserialize, Serialize};
    use vespera::Schema;

    fn limit() -> u32 {
        10
    }

    #[derive(Serialize, Deserialize, Clone, Schema)]
    pub struct Page {
        #[serde(default = "limit")]
        pub limit: u32,
    }
}

/// `first::Page`'s schema, read BEFORE `second::Page` is derived: schema
/// storage is keyed by the Rust name, so a later `schema!(first::Page)`
/// would read `second::Page` (the last `Page` derived).
fn first_page_schema() -> vespera::schema::Schema {
    vespera::schema!(first::Page)
}

mod second {
    use serde::{Deserialize, Serialize};
    use vespera::Schema;

    fn limit() -> u32 {
        20
    }

    #[derive(Serialize, Deserialize, Clone, Schema)]
    pub struct Page {
        #[serde(default = "limit")]
        pub limit: u32,
    }
}

#[test]
fn each_page_resolves_its_own_default_fn() {
    let first: first::Page = serde_json::from_str("{}").unwrap();
    let second: second::Page = serde_json::from_str("{}").unwrap();
    assert_eq!((first.limit, second.limit), (10, 20));
    // serde resolves those on its own; the derive's statically extracted
    // OpenAPI `default` is what this file is about.  Each struct must carry
    // its OWN module's default: a derive that resolved one module's `limit`
    // for both structs (either direction) fails one of these.
    assert_eq!(limit_default(&first_page_schema()), json!(10));
    assert_eq!(limit_default(&vespera::schema!(second::Page)), json!(20));
}

fn limit_default(schema: &vespera::schema::Schema) -> serde_json::Value {
    let properties = schema.properties.as_ref().expect("properties");
    serde_json::to_value(&properties["limit"]).unwrap()["default"].clone()
}

use serde_json::json;
