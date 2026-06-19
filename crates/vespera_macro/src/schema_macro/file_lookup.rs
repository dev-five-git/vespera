//! File system operations for finding struct definitions.

mod fk;
mod lookup;

#[allow(unused_imports)]
pub use fk::find_fk_column_from_target_entity;
#[allow(unused_imports)]
pub use lookup::{
    collect_rs_files_recursive, file_path_to_module_path, find_model_from_schema_path,
    find_struct_from_path_detailed, find_struct_from_schema_path,
};
#[cfg(test)]
pub use lookup::{find_struct_by_name_in_all_files, find_struct_from_path};

#[cfg(test)]
mod schema_type_lookup_tests {
    use std::collections::HashMap;

    use quote::quote;
    use serial_test::serial;

    use crate::metadata::StructMetadata;
    use crate::schema_macro::{SchemaTypeInput, generate_schema_type_code};

    #[test]
    #[serial]
    fn test_generate_schema_type_code_qualified_path_file_lookup_success() {
        // Tests: qualified path found via file lookup, module_path used when source is empty
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create user.rs with Model struct
        let user_model = r"
pub struct Model {
    pub id: i32,
    pub name: String,
    pub email: String,
}
";
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Use qualified path - file lookup should succeed
        let tokens = quote!(UserSchema from crate::models::user::Model);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let storage: HashMap<String, StructMetadata> = HashMap::new(); // Empty storage - force file lookup

        let result = generate_schema_type_code(&input, &storage);

        // Restore CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("UserSchema"));
        assert!(output.contains("id"));
        assert!(output.contains("name"));
        assert!(output.contains("email"));
    }

    #[test]
    #[serial]
    fn test_generate_schema_type_code_simple_name_file_lookup_fallback() {
        // Tests: simple name (not in storage) found via file lookup with schema_name hint
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create user.rs with Model struct
        let user_model = r"
pub struct Model {
    pub id: i32,
    pub username: String,
}
";
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Use simple name with schema_name hint - file lookup should find it via hint
        // name = "UserSchema" provides hint to look in user.rs
        let tokens = quote!(Schema from Model, name = "UserSchema");
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let storage: HashMap<String, StructMetadata> = HashMap::new(); // Empty storage - force file lookup

        let result = generate_schema_type_code(&input, &storage);

        // Restore CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_ok());
        let (tokens, metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("Schema"));
        assert!(output.contains("id"));
        assert!(output.contains("username"));
        // Metadata should be returned for custom name
        assert!(metadata.is_some());
        assert_eq!(metadata.unwrap().name, "UserSchema");
    }

    // ============================================================
    // Tests for HasMany explicit pick with inline type
    // ============================================================

    #[test]
    #[serial]
    fn test_generate_schema_type_code_has_many_explicit_pick_inline_type() {
        // Tests: HasMany is explicitly picked, inline type is generated
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create memo.rs with Model struct (the target of HasMany)
        let memo_model = r"
pub struct Model {
    pub id: i32,
    pub title: String,
    pub content: String,
}
";
        std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

        // Create user.rs with Model struct that has HasMany relation
        let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub memos: HasMany<super::memo::Entity>,
}
"#;
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Explicitly pick HasMany field - should generate inline type
        let tokens =
            quote!(UserSchema from crate::models::user::Model, pick = ["id", "name", "memos"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let storage: HashMap<String, StructMetadata> = HashMap::new();

        let result = generate_schema_type_code(&input, &storage);

        // Restore CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should have inline type definition for memos
        assert!(output.contains("UserSchema"));
        assert!(output.contains("memos"));
        // Inline type should be Vec<InlineType>
        assert!(output.contains("Vec <"));
    }

    #[test]
    #[serial]
    fn test_generate_schema_type_code_has_many_explicit_pick_file_not_found() {
        // Tests: HasMany is explicitly picked but target file not found - should skip field
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create user.rs with Model struct that has HasMany to nonexistent model
        let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub items: HasMany<super::nonexistent::Entity>,
}
"#;
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Explicitly pick HasMany field - file not found, should skip
        let tokens =
            quote!(UserSchema from crate::models::user::Model, pick = ["id", "name", "items"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let storage: HashMap<String, StructMetadata> = HashMap::new();

        let result = generate_schema_type_code(&input, &storage);

        // Restore CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // items field should be skipped (file not found for inline type)
        assert!(!output.contains("items"));
        // But other fields should exist
        assert!(output.contains("id"));
        assert!(output.contains("name"));
    }
    #[test]
    #[serial]
    fn test_generate_schema_type_code_qualified_path_with_nonempty_module_path() {
        // Tests: qualified path with explicit module segments that are not empty
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create user.rs
        let user_model = r"
pub struct Model {
    pub id: i32,
    pub name: String,
}
";
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // crate::models::user::Model - this is a qualified path
        // extract_module_path should return ["crate", "models", "user"]
        // So the if source_module_path.is_empty() check should be false
        let tokens = quote!(UserSchema from crate::models::user::Model);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let storage: HashMap<String, StructMetadata> = HashMap::new();

        let result = generate_schema_type_code(&input, &storage);

        // Restore CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("UserSchema"));
    }

    #[test]
    #[serial]
    fn test_generate_schema_type_code_cross_module_json_alias_uses_public_path() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        let routes_dir = src_dir.join("routes");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::create_dir_all(&routes_dir).unwrap();

        let json_case_model = r#"
use sea_orm::entity::prelude::*;

#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "json_case")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub payload: Json,
}

impl ActiveModelBehavior for ActiveModel {}
"#;
        std::fs::write(models_dir.join("json_case.rs"), json_case_model).unwrap();
        std::fs::write(
            routes_dir.join("json_case.rs"),
            "vespera::schema_type!(RouteJsonCaseSchema from crate::models::json_case::Model);",
        )
        .unwrap();

        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let tokens = quote!(RouteJsonCaseSchema from crate::models::json_case::Model);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let storage: HashMap<String, StructMetadata> = HashMap::new();
        let result = generate_schema_type_code(&input, &storage);

        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("pub payload : vespera :: serde_json :: Value"));
        assert!(!output.contains("crate :: models :: json_case :: Json"));
    }
}
