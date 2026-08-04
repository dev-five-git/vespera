//! `SeaORM` and Chrono type conversions.

mod attrs;
mod conversion;
mod relations;

pub use attrs::{
    extract_belongs_to_from_field, extract_relation_enum, extract_sea_orm_default_value,
    has_sea_orm_primary_key, is_sql_function_default,
};
pub use conversion::convert_type_with_chrono;
#[cfg(test)]
pub use relations::is_field_optional_in_struct;
pub use relations::{RelationFieldInfo, convert_relation_type_to_schema_with_info};

// Circular-relation integration tests live here because relation
// conversion (`convert_relation_type_to_schema_with_info`) is the
// seaorm-owned behavior they exercise end-to-end.
#[cfg(test)]
mod circular_relation_tests {
    use std::collections::HashMap;

    use quote::quote;
    use serial_test::serial;

    use crate::metadata::StructMetadata;
    use crate::schema_macro::{SchemaTypeInput, generate_schema_type_code};

    // ============================================================
    // Tests for BelongsTo/HasOne circular reference inline types
    // ============================================================

    #[test]
    #[serial]
    fn test_generate_schema_type_code_belongs_to_circular_inline_optional() {
        // Tests: BelongsTo with circular reference, optional field (is_optional = true)
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create user.rs with Model that references memo (circular)
        let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub memo: BelongsTo<super::memo::Entity>,
}
"#;
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Create memo.rs with Model that references user (completing the circle)
        let memo_model = r#"
#[sea_orm(table_name = "memos")]
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Generate schema from memo - has BelongsTo user which has circular ref back
        let tokens = quote!(MemoSchema from crate::models::memo::Model);
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
        // Should have inline type definition for circular relation
        assert!(output.contains("MemoSchema"));
        assert!(output.contains("user"));
        // BelongsTo is optional by default, so should have Option<Box<...>>
        assert!(output.contains("Option < Box <"));
    }

    #[test]
    #[serial]
    fn test_generate_schema_type_code_has_one_circular_inline_required() {
        // Tests: HasOne with circular reference, required field (is_optional = false)
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create profile.rs with Model that references user (circular)
        let profile_model = r#"
#[sea_orm(table_name = "profiles")]
pub struct Model {
    pub id: i32,
    pub bio: String,
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("profile.rs"), profile_model).unwrap();

        // Create user.rs with Model that has HasOne profile
        // HasOne with required FK becomes required (non-optional)
        let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub profile_id: i32,
    #[sea_orm(has_one = "super::profile::Entity", from = "profile_id")]
    pub profile: HasOne<super::profile::Entity>,
}
"#;
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Generate schema from user - has HasOne profile which has circular ref back
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
        // Should have inline type definition for circular relation
        assert!(output.contains("UserSchema"));
        assert!(output.contains("profile"));
        // HasOne with required FK should have Box<...> (not Option<Box<...>>)
        assert!(output.contains("Box <"));
    }

    #[test]
    #[serial]
    fn test_generate_schema_type_code_belongs_to_circular_inline_required_file() {
        // Tests: BelongsTo with circular reference AND required FK (is_optional = false)
        // This requires file-based lookup with:
        // 1. #[sea_orm(from = "required_fk")] where required_fk is NOT Option<T>
        // 2. Circular reference between two models
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create user.rs with Model that references memo (circular)
        let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub memo_id: i32,
    #[sea_orm(belongs_to, from = "memo_id", to = "id")]
    pub memo: BelongsTo<super::memo::Entity>,
}
"#;
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Create memo.rs with Model that references user (completing the circle)
        // Note: using flag-style `belongs_to` with `from = "user_id"`
        let memo_model = r#"
#[sea_orm(table_name = "memos")]
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

        // Save original CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Generate schema from memo - has BelongsTo user which has circular ref back
        // The user_id field is required (not Option), so is_optional = false
        // This should generate Box<...> instead of Option<Box<...>>
        let tokens = quote!(MemoSchema from crate::models::memo::Model);
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

        assert!(result.is_ok(), "Should generate schema: {:?}", result.err());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should have inline type definition for circular relation
        assert!(
            output.contains("MemoSchema"),
            "Should contain MemoSchema: {output}"
        );
        assert!(
            output.contains("user"),
            "Should contain user field: {output}"
        );
        // BelongsTo with required FK (user_id: i32) should generate Box<...> not Option<Box<...>>
        assert!(
            output.contains("pub user : Box <"),
            "BelongsTo with required FK should generate Box<>, not Option<Box<>>. Output: {output}"
        );
    }

    #[test]
    fn test_seaorm_relation_required_fk_directly() {
        // Test the convert_relation_type_to_schema_with_info function directly
        // to verify is_optional = false when FK is required
        use crate::schema_macro::seaorm::{
            convert_relation_type_to_schema_with_info, extract_belongs_to_from_field,
            is_field_optional_in_struct,
        };

        // Use the same attribute format that works in seaorm tests: belongs_to (flag), not belongs_to = "..."
        let struct_def = r#"
#[sea_orm(table_name = "memos")]
pub struct Model {
    pub id: i32,
    pub user_id: i32,
    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        let parsed_struct: syn::ItemStruct = syn::parse_str(struct_def).unwrap();

        // Get the user field
        let syn::Fields::Named(fields_named) = &parsed_struct.fields else {
            panic!("Expected named fields")
        };

        let user_field = fields_named
            .named
            .iter()
            .find(|f| f.ident.as_ref().is_some_and(|i| i == "user"))
            .expect("user field not found");

        // Debug: Check if extract_belongs_to_from_field works
        let fk_field = extract_belongs_to_from_field(&user_field.attrs);
        assert_eq!(
            fk_field,
            Some("user_id".to_string()),
            "Should extract FK field from attribute"
        );

        // Debug: Check if is_field_optional_in_struct works
        let is_fk_optional = is_field_optional_in_struct(&parsed_struct, "user_id");
        assert!(!is_fk_optional, "user_id: i32 should not be optional");

        let result = convert_relation_type_to_schema_with_info(
            &user_field.ty,
            &user_field.attrs,
            &parsed_struct,
            &[
                "crate".to_string(),
                "models".to_string(),
                "memo".to_string(),
            ],
            user_field.ident.clone().unwrap(),
        );

        assert!(result.is_some(), "Should convert BelongsTo relation");
        let (_, rel_info) = result.unwrap();
        assert_eq!(rel_info.relation_type, "BelongsTo");
        // The FK field user_id is i32 (not Option), so is_optional should be false
        assert!(
            !rel_info.is_optional,
            "BelongsTo with required FK (user_id: i32) should have is_optional = false"
        );
    }

    #[test]
    fn test_extract_belongs_to_from_field_with_equals_value() {
        // Test that extract_belongs_to_from_field works with belongs_to = "..." format
        use crate::schema_macro::seaorm::extract_belongs_to_from_field;

        // Format 1: belongs_to (flag style) - known to work
        let attrs1: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to, from = "user_id", to = "id")]
        )];
        let result1 = extract_belongs_to_from_field(&attrs1);
        assert_eq!(
            result1,
            Some("user_id".to_string()),
            "Flag style should work"
        );

        // Format 2: belongs_to = "..." (value style) - testing this
        let attrs2: Vec<syn::Attribute> = vec![syn::parse_quote!(
            #[sea_orm(belongs_to = "super::user::Entity", from = "user_id", to = "id")]
        )];
        let result2 = extract_belongs_to_from_field(&attrs2);
        assert_eq!(
            result2,
            Some("user_id".to_string()),
            "Value style should also work"
        );
    }
}
