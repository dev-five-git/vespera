use serial_test::serial;
use tempfile::TempDir;

use super::*;
use crate::schema_macro::file_cache::bump_epoch;

struct RestoreManifest(Option<String>);

impl Drop for RestoreManifest {
    fn drop(&mut self) {
        // SAFETY: every test mutating the process environment is serialized.
        unsafe {
            match self.0.take() {
                Some(value) => std::env::set_var("CARGO_MANIFEST_DIR", value),
                None => std::env::remove_var("CARGO_MANIFEST_DIR"),
            }
        }
    }
}

#[test]
#[serial]
fn fingerprint_without_manifest_uses_path_only() {
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::remove_var("CARGO_MANIFEST_DIR") };
    bump_epoch();

    FILE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        assert_ne!(path_lookup_fingerprint(&mut cache, "crate::Model"), 0);
    });
}

#[test]
#[serial]
fn struct_and_fk_lookups_revalidate_unchanged_entries_across_epochs() {
    let temp = TempDir::new().unwrap();
    let model_dir = temp.path().join("src/models");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(
        model_dir.join("user.rs"),
        r#"pub struct Model { pub user_id: i32 }
           pub enum Relation { #[sea_orm(belongs_to = "Entity", from = "Column::UserId", to = "Column::Id")] User }"#,
    )
    .unwrap();
    let _restore = RestoreManifest(std::env::var("CARGO_MANIFEST_DIR").ok());
    // SAFETY: this serialized test restores the process environment through RAII.
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp.path()) };

    bump_epoch();
    let first =
        get_struct_from_schema_path("crate::models::user::Model").expect("fixture model resolves");
    assert!(first.definition.contains("user_id"));
    let first_fk = get_fk_column("crate::models::user::Schema", "User");

    bump_epoch();
    let second = get_struct_from_schema_path("crate::models::user::Model")
        .expect("unchanged fixture remains cached");
    let second_fk = get_fk_column("crate::models::user::Schema", "User");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first_fk, second_fk);
}

#[test]
fn module_path_lookup_returns_cached_value() {
    let schema_path = quote::quote!(crate::models::user::Schema);
    let expected = vec!["crate", "models", "user"];
    assert_eq!(get_module_path_from_schema_path(&schema_path), expected);
    assert_eq!(get_module_path_from_schema_path(&schema_path), expected);
}
