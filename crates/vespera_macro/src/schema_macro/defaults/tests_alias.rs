use super::*;

/// R24-SM1-L2: a bare `Date` / `Time` / `Uuid` names the chrono / uuid type
/// only when the module does not define or import another one.
#[test]
fn default_alias_is_judged_through_the_module_scope() {
    let temp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub enum Time { A }\nuse chrono::NaiveDate as Date;\nuse sea_orm::prelude;\n\
         use crate::types as t;\npub struct DateTime(pub i64);\npub struct NaiveTime;\n",
    )
    .unwrap();
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate".to_string()];
    let judge = |ty: &str| default_alias_is_external(&syn::parse_str(ty).unwrap(), &module);
    // Defined here: not the chrono type.
    assert!(!judge("Time"));
    // Imported from chrono.
    assert!(judge("Date"));
    // Not in scope at all (a prelude or glob name): trusted as before.
    assert!(judge("Uuid"));
    // A path written out in full is judged by its root.
    assert!(judge("chrono::NaiveTime"));
    assert!(!judge("crate::models::Uuid"));
    assert!(judge("uuid::Uuid"));
    // The chrono re-export of vespera is the chrono type too (R36-SM1-M1).
    assert!(judge("vespera::chrono::NaiveDate"));
    assert!(judge("vespera::chrono::NaiveDateTime"));
    assert!(!judge("vespera::models::Date"));
    // Other names are never judged.
    assert!(judge("NaiveDate"));
    // R25-SM1-L1: a multi-segment path is judged through the module's
    // imports too, not by its literal root.
    assert!(judge("prelude::Date"));
    assert!(!judge("t::Date"));
    // A multi-segment path with an unknown root keeps its literal root.
    assert!(!judge("other::Date"));
    // R25-SM1-L2: a user type named like a chrono type is not judged as one.
    assert!(!judge("DateTime"));
    assert!(!judge("NaiveTime"));
}

/// R32-SM1-L2: a bare `Decimal` is judged like the chrono / uuid names: a
/// module's own `Decimal` gets no generated default, an imported
/// `rust_decimal::Decimal` does.
#[test]
fn decimal_default_is_judged_through_the_module_scope() {
    let temp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub struct Decimal(pub i64);\nuse rust_decimal::Decimal as Price;\n",
    )
    .unwrap();
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate".to_string()];
    let judge = |ty: &str| default_alias_is_external(&syn::parse_str(ty).unwrap(), &module);
    // Defined in the module: not the decimal type.
    assert!(!judge("Decimal"));
    // Written out in full, the root decides.
    assert!(judge("rust_decimal::Decimal"));
    assert!(!judge("crate::models::Decimal"));
}

#[test]
fn decimal_default_needs_an_external_decimal_type() {
    let temp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub struct Decimal(pub i64);\nuse rust_decimal::Decimal as Price;\n",
    )
    .unwrap();
    let _env = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", temp.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate".to_string()];
    let user_decimal: syn::Type = syn::parse_str("Decimal").unwrap();
    let external: syn::Type = syn::parse_str("rust_decimal::Decimal").unwrap();
    // A literal default needs `FromStr`: the module's own `Decimal` has none.
    assert!(!is_parseable_type(&user_decimal, &module));
    assert!(is_parseable_type(&external, &module));
    // The generated default follows the same rule.
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "0")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs_in(
        &attrs,
        &struct_name,
        "price",
        &user_decimal,
        &user_decimal,
        false,
        1,
        &module,
        &mut fns,
    );
    assert!(serde.is_empty() && schema.is_empty() && fns.is_empty());
    let mut fns = Vec::new();
    let (serde, _schema) = generate_sea_orm_default_attrs_in(
        &attrs,
        &struct_name,
        "price",
        &external,
        &external,
        false,
        1,
        &module,
        &mut fns,
    );
    assert!(serde.to_string().contains("default_Test_price"));
    assert_eq!(fns.len(), 1);
}
