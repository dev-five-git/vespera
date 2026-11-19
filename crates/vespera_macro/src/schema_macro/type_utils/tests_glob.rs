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
        "pub mod toml_glob;\npub mod std_glob;\npub mod decimal_glob;",
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
    // R29-SM4-01: `Decimal` is no primitive, so this glob supplies it.
    std::fs::write(
        src.join("models").join("decimal_glob.rs"),
        "use rust_decimal::*;\npub struct Holder { pub total: Decimal }",
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
#[case::decimal_is_not_a_prelude_name("decimal_glob", "Decimal", "rust_decimal :: Decimal")]
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

/// R22-SM4-M1: with a SeaORM glob and an external glob both imported, a name
/// the SeaORM prelude does not export (`Local`, `Utc`, `NaiveDate`) is the
/// external glob's, not the chrono alias's.
#[rstest]
#[case::local("Local", "my_domain :: Local")]
#[case::utc("Utc", "my_domain :: Utc")]
#[case::naive_date("NaiveDate", "my_domain :: NaiveDate")]
fn sea_orm_glob_does_not_shadow_names_outside_its_prelude(
    #[case] ty: &str,
    #[case] expected: &str,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod domain;").unwrap();
    std::fs::write(
        src.join("models").join("domain.rs"),
        "use sea_orm::prelude::*;\nuse my_domain::*;\npub struct Holder { pub v: u8 }",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", "domain"].map(String::from);
    let ty: syn::Type = syn::parse_str(ty).unwrap();
    assert_eq!(
        resolve_type_with_params(&ty, &module, &[]).to_string(),
        expected
    );
}

/// R22-SM4-L1: with two external globs and no other meaning for a name, the
/// macro does not guess which glob supplies it: a spanned error asks for an
/// explicit import instead.
#[test]
fn name_from_two_external_globs_asks_for_an_explicit_import() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod two;").unwrap();
    std::fs::write(
        src.join("models").join("two.rs"),
        "use toml::*;\nuse my_domain::*;\npub struct Holder { pub w: u8 }",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", "two"].map(String::from);
    let ty: syn::Type = syn::parse_str("Widget").unwrap();
    let output = resolve_type_with_params(&ty, &module, &[]).to_string();
    assert!(
        output.contains("compile_error") && output.contains("import it explicitly"),
        "{output}"
    );
}

/// R24-SM4-L1: a chrono-only name (`NaiveDate`) stays the chrono alias with two
/// chrono-family external globs (valid input used to be a hard error); the
/// globs cannot give it another meaning.
#[test]
fn chrono_only_name_from_two_chrono_family_globs_keeps_its_alias() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod dated;").unwrap();
    std::fs::write(
        src.join("models").join("dated.rs"),
        "use chrono::*;\nuse chrono::naive::*;\npub struct Holder { pub d: u8 }",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", "dated"].map(String::from);
    let ty: syn::Type = syn::parse_str("NaiveDate").unwrap();
    let output = resolve_type_with_params(&ty, &module, &[]).to_string();
    assert!(
        output.contains("chrono :: NaiveDate") && !output.contains("compile_error"),
        "{output}"
    );
}

/// R25-SM4-L1: a chrono-only name (`NaiveDate`) with a non-chrono external glob
/// (`my_domain` may export its own `NaiveDate`) asks for an explicit import
/// rather than taking the chrono alias.
#[test]
fn chrono_only_name_with_a_non_chrono_glob_asks_for_an_explicit_import() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod dated;").unwrap();
    std::fs::write(
        src.join("models").join("dated.rs"),
        "use toml::*;\nuse my_domain::*;\npub struct Holder { pub d: u8 }",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", "dated"].map(String::from);
    let ty: syn::Type = syn::parse_str("NaiveDate").unwrap();
    let output = resolve_type_with_params(&ty, &module, &[]).to_string();
    assert!(
        output.contains("compile_error") && output.contains("import it explicitly"),
        "{output}"
    );
}

/// R23-SM4-L1: a name that a SeaORM alias also knows (`Utc`) is not exempt
/// from the several-external-globs error: the alias is no reason to pick one
/// of the globs silently (it used to emit `std :: collections :: Utc`).
#[test]
fn alias_named_type_from_two_external_globs_asks_for_an_explicit_import() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod stamped;").unwrap();
    std::fs::write(
        src.join("models").join("stamped.rs"),
        "use std::collections::*;\nuse my_domain::*;\npub struct Holder { pub v: u8 }",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", "stamped"].map(String::from);
    let ty: syn::Type = syn::parse_str("Utc").unwrap();
    let output = resolve_type_with_params(&ty, &module, &[]).to_string();
    assert!(
        output.contains("compile_error") && output.contains("import it explicitly"),
        "{output}"
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
