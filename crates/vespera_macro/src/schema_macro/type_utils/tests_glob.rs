//! External glob imports of the source module (R20-S4-L1, round 20): a name
//! the module neither defines nor imports comes from its glob.

use rstest::rstest;

use super::*;

/// `models/toml_glob.rs` has `use toml::*;`, `models/std_glob.rs` has
/// `use std::collections::*;`, so each name has exactly one glob to come from.
fn glob_crate() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(
        src.join("models.rs"),
        "pub mod toml_glob;\npub mod std_glob;",
    )
    .unwrap();
    std::fs::write(
        src.join("models").join("toml_glob.rs"),
        "use toml::*;\npub struct Holder { pub v: Value }",
    )
    .unwrap();
    std::fs::write(
        src.join("models").join("std_glob.rs"),
        "use std::collections::*;\npub struct Holder { pub m: HashMap<String, i32> }",
    )
    .unwrap();
    dir
}

#[rstest]
#[case::toml_value("toml_glob", "Value", "toml :: Value")]
#[case::std_hash_map(
    "std_glob",
    "HashMap<String, i32>",
    "std :: collections :: HashMap < String , i32 >"
)]
fn external_glob_supplies_names_the_module_does_not_define(
    #[case] module: &str,
    #[case] ty: &str,
    #[case] expected: &str,
) {
    let dir = glob_crate();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", module].map(String::from);
    let ty: syn::Type = syn::parse_str(ty).unwrap();
    assert_eq!(
        resolve_type_with_params(&ty, &module, &[]).to_string(),
        expected
    );
}

/// R21-B4-M1 / M2 (round 21): with the SeaORM prelude and an external glob
/// both imported, a SeaORM prelude name is SeaORM's (the external glob took
/// `DateTimeUtc`, `Uuid`, `Json`), and `char` / `str` are never a glob's.
#[rstest]
#[case::date_time_utc(
    "DateTimeUtc",
    "vespera :: chrono :: DateTime < vespera :: chrono :: Utc >"
)]
#[case::uuid("Uuid", "sea_orm :: prelude :: Uuid")]
#[case::json("Json", "vespera :: serde_json :: Value")]
#[case::char_primitive("char", "char")]
#[case::str_primitive("str", "str")]
fn sea_orm_prelude_and_external_glob_together(#[case] ty: &str, #[case] expected: &str) {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod both;").unwrap();
    std::fs::write(
        src.join("models").join("both.rs"),
        "use sea_orm::prelude::*;\nuse std::collections::*;\npub struct Holder { pub v: u8 }",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", "both"].map(String::from);
    let ty: syn::Type = syn::parse_str(ty).unwrap();
    assert_eq!(
        resolve_type_with_params(&ty, &module, &[]).to_string(),
        expected
    );
}
