//! A stored `#[derive(Schema)]` entry whose item left its file (renamed,
//! moved to another file, file deleted) in a long-lived proc-macro server
//! (rust-analyzer) no longer blocks a new item of that name with a false
//! duplicate-name error; a stored item still in its (possibly edited) file
//! keeps the strict error a clean build raises.

use std::path::Path;

use super::*;
use crate::schema_macro::file_cache::bump_epoch;

fn identity(file: &Path, line: usize) -> String {
    format!("{}::User@{line}:11", file.display())
}

fn remove(key: &str) {
    let mut guard = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(bucket) = guard.get_mut(&current_crate_key()) {
        Arc::make_mut(bucket).remove(key);
    }
}

#[rstest::rstest]
#[case::unchanged(Some("pub struct User { id: i32 }\n"), false)]
#[case::edited_still_there(Some("pub struct User { id: i64, extra: u8 }\n"), false)]
#[case::renamed(Some("pub struct Account { id: i32 }\n"), true)]
#[case::moved_out(Some("// User moved to b.rs\n"), true)]
#[case::file_deleted(None, true)]
fn stale_entry_of_a_changed_file(#[case] first_file_after: Option<&str>, #[case] accepted: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let first = dir.path().join("a.rs");
    let second = dir.path().join("b.rs");
    std::fs::write(&first, "pub struct User { id: i32 }\n").unwrap();
    std::fs::write(&second, "\npub struct User { name: String }\n").unwrap();
    let key = format!("__test_stale_entry_{accepted}_{first_file_after:?}__");
    remove(&key);
    bump_epoch();
    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct User { id: i32 }".to_string())
                .with_source_identity(identity(&first, 1)),
        )
        .is_ok()
    );

    if first_file_after != Some("pub struct User { id: i32 }\n") {
        match first_file_after {
            Some(source) => std::fs::write(&first, source).unwrap(),
            None => std::fs::remove_file(&first).unwrap(),
        }
    }
    bump_epoch();
    let result = register_schema(
        key.clone(),
        StructMetadata::new(key.clone(), "struct User { name: String }".to_string())
            .with_source_identity(identity(&second, 2)),
    );
    assert_eq!(result.is_ok(), accepted);
    assert_eq!(
        current_crate_schemas()[&key].definition.contains("name"),
        accepted
    );
    remove(&key);
}

/// R25-SM0-L1: a variant of a same-named item whose item was deleted from its
/// file (the file changed since the registration) is no longer a variant, so
/// `schema!` raises the missing-derive error a clean build raises instead of
/// reading the stale definition. The other module's variant still resolves.
#[test]
fn variant_of_a_deleted_item_is_no_longer_a_variant() {
    const SOURCE: &str = "pub struct VariantGone { pub limit: u32 }\n";
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod first;\npub mod second;\n").unwrap();
    let first = src.join("first.rs");
    let second = src.join("second.rs");
    std::fs::write(&first, SOURCE).unwrap();
    std::fs::write(&second, SOURCE).unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    bump_epoch();

    let metadata = |file: &Path| {
        StructMetadata::new("VariantGone".to_string(), SOURCE.trim().to_string())
            .with_source_identity(format!("{}::VariantGone@1:11", file.display()))
    };
    let first_module = ["crate", "first"].map(String::from);
    let second_module = ["crate", "second"].map(String::from);
    assert!(register_schema("VariantGone".to_string(), metadata(&first)).is_ok());
    assert!(register_schema("VariantGone".to_string(), metadata(&second)).is_ok());
    assert!(variants::in_module("VariantGone", &second_module).is_some());

    std::fs::write(&second, "// VariantGone was deleted\n").unwrap();
    bump_epoch();
    assert!(variants::in_module("VariantGone", &second_module).is_none());
    assert!(variants::in_module("VariantGone", &first_module).is_some());
}
