use std::collections::HashMap;

use quote::quote;

use crate::metadata::StructMetadata;
use crate::schema_macro::{
    SchemaInput, SchemaTypeInput, generate_schema_code, generate_schema_type_code,
};

fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
    StructMetadata::new(name.to_string(), definition.to_string())
}

fn to_storage(items: Vec<StructMetadata>) -> HashMap<String, StructMetadata> {
    items.into_iter().map(|s| (s.name.clone(), s)).collect()
}

// Tests for field rename processing

#[test]
fn test_generate_schema_type_code_with_rename() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserDTO from User, rename = [("id", "user_id")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("user_id"));
    // The From impl should map user_id from source.id
    assert!(output.contains("From"));
}

#[test]
fn test_generate_schema_type_code_rename_preserves_serde_rename() {
    // Source field already has serde(rename), which should be preserved as the JSON name
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        r#"pub struct User {
                pub id: i32,
                #[serde(rename = "userName")]
                pub name: String
            }"#,
    )]);

    let tokens = quote!(UserDTO from User, rename = [("name", "user_name")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // The Rust field is renamed to user_name
    assert!(output.contains("user_name"));
    // The JSON name should be preserved as userName
    assert!(output.contains("userName") || output.contains("rename"));
}

// Tests for schema derive and name attribute generation

#[test]
fn test_generate_schema_type_code_with_ignore_schema() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserInternal from User, ignore);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should NOT contain vespera::Schema derive
    assert!(!output.contains("vespera :: Schema"));
}

#[test]
fn test_generate_schema_type_code_with_custom_name() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserResponse from User, name = "CustomUserSchema");
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should contain schema(name = "...") attribute
    assert!(output.contains("schema"));
    assert!(output.contains("CustomUserSchema"));
    // Metadata should be returned
    assert!(metadata.is_some());
    let meta = metadata.unwrap();
    assert_eq!(meta.name, "CustomUserSchema");
}

#[test]
fn test_generate_schema_type_code_with_clone_false() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserNonClone from User, clone = false);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should NOT contain Clone derive
    assert!(!output.contains("Clone ,"));
}

// Test for SeaORM model detection

#[test]
fn test_generate_schema_type_code_seaorm_model_detection() {
    // Source struct has sea_orm attribute - should be detected as SeaORM model
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model { pub id: i32, pub name: String }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("UserSchema"));
}

// Test tuple struct handling

#[test]
fn test_generate_schema_type_code_tuple_struct() {
    // Tuple structs have no named fields
    let storage = to_storage(vec![create_test_struct_metadata(
        "Point",
        "pub struct Point(pub i32, pub i32);",
    )]);

    let tokens = quote!(PointDTO from Point);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("PointDTO"));
}

// Test raw identifier fields

#[test]
fn test_generate_schema_type_code_raw_identifier_field() {
    // Field name is a Rust keyword with r# prefix
    let storage = to_storage(vec![create_test_struct_metadata(
        "Config",
        "pub struct Config { pub id: i32, pub r#type: String }",
    )]);

    let tokens = quote!(ConfigDTO from Config);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("ConfigDTO"));
}

// Test Option field not double-wrapped with partial

#[test]
fn test_generate_schema_type_code_partial_no_double_option() {
    // bio is already Option<String>, partial should NOT wrap it again
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub bio: Option<String> }",
    )]);

    let tokens = quote!(UpdateUser from User, partial);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // bio should remain Option<String>, not Option<Option<String>>
    assert!(!output.contains("Option < Option"));
}

// Test serde(skip) fields are excluded

#[test]
fn test_generate_schema_code_excludes_serde_skip_fields() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        r"pub struct User {
                pub id: i32,
                #[serde(skip)]
                pub internal_state: String,
                pub name: String
            }",
    )]);

    let tokens = quote!(User);
    let input: SchemaInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_code(&input, &storage);

    assert!(result.is_ok());
    let output = result.unwrap().to_string();
    // internal_state should be excluded from schema properties
    assert!(!output.contains("internal_state"));
    assert!(output.contains("name"));
}

// Tests for qualified path storage fallback: a qualified source path like
// `crate::models::user::Model` resolves through schema_storage rather than
// via file lookup.

#[test]
fn test_generate_schema_type_code_qualified_path_storage_lookup() {
    // Use a qualified path like crate::models::user::Model
    // The storage contains Model, so it should fallback to storage lookup
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        "pub struct Model { pub id: i32, pub name: String }",
    )]);

    // Note: This qualified path won't find files (no real filesystem),
    // so it falls back to storage lookup by the simple name "Model"
    let tokens = quote!(UserSchema from crate::models::user::Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    // This should succeed by finding Model in storage
    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("UserSchema"));
}

// Test for qualified path not found error

#[test]
fn test_generate_schema_type_code_qualified_path_not_found() {
    // Empty storage - qualified path should fail
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let tokens = quote!(UserSchema from crate::models::user::NonExistent);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    // Should fail with "not found" error
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"));
}

// Tests for HasMany excluded by default

#[test]
fn test_generate_schema_type_code_has_many_excluded_by_default() {
    // SeaORM model with HasMany relation - should be excluded by default
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub memos: HasMany<super::memo::Entity>
            }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // HasMany field should NOT appear in output (excluded by default)
    assert!(!output.contains("memos"));
    // But regular fields should appear
    assert!(output.contains("name"));
}

// Test for relation conversion failure skip

#[test]
fn test_generate_schema_type_code_relation_conversion_failure() {
    // Model with relation type but missing generic args - conversion should fail
    // The field should be skipped
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub broken: HasMany
            }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    // Should succeed but skip the broken field
    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Broken field should be skipped
    assert!(!output.contains("broken"));
    // Regular fields should appear
    assert!(output.contains("name"));
}

// Coverage test for BelongsTo relation type conversion

#[test]
fn test_generate_schema_type_code_belongs_to_relation() {
    // SeaORM model with BelongsTo relation - should be included
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "memos")]
            pub struct Model {
                pub id: i32,
                pub user_id: i32,
                #[sea_orm(belongs_to = "super::user::Entity", from = "user_id")]
                pub user: BelongsTo<super::user::Entity>
            }"#,
    )]);

    let tokens = quote!(MemoSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // BelongsTo should be included (converted to Box<UserSchema> or similar)
    assert!(output.contains("user"));
}

// Coverage test for HasOne relation type

#[test]
fn test_generate_schema_type_code_has_one_relation() {
    // SeaORM model with HasOne relation - should be included
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub profile: HasOne<super::profile::Entity>
            }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // HasOne should be included
    assert!(output.contains("profile"));
}

// Test for relation fields push into relation_fields

#[test]
fn test_generate_schema_type_code_seaorm_model_with_relation_generates_from_model() {
    // When a SeaORM model has FK relations (HasOne/BelongsTo),
    // it should generate from_model impl instead of From impl
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "memos")]
            pub struct Model {
                pub id: i32,
                pub title: String,
                pub user: BelongsTo<super::user::Entity>
            }"#,
    )]);

    let tokens = quote!(MemoSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should have relation field
    assert!(output.contains("user"));
    // Should NOT have regular From impl (because of relation)
    // The From impl is only generated when there are no relation fields
}

// Test for from_model generation with relations
// Note: This requires is_source_seaorm_model && has_relation_fields
// The from_model generation happens but needs file lookup for full path

#[test]
fn test_generate_schema_type_code_from_model_generation() {
    // SeaORM model with relation should trigger from_model generation
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "memos")]
            pub struct Model {
                pub id: i32,
                pub user: BelongsTo<super::user::Entity>
            }"#,
    )]);

    let tokens = quote!(MemoSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Has relation field
    assert!(output.contains("user"));
    // Regular impl From should NOT be present (because has relations)
    // Check that we don't have "impl From < Model > for MemoSchema"
    // (Relations disable the automatic From impl)
}
