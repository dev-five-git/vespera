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
