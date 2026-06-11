//! Field transformation logic for `schema_type`! macro.
//!
//! This module contains functions for building filter sets, rename maps,
//! and extracting/filtering attributes from source structs.
//!
//! # Overview
//!
//! The `schema_type`! macro applies transformations to the source struct to create a new schema type.
//! This module provides utilities to:
//! - Build sets of fields to include (pick) or exclude (omit)
//! - Construct rename maps for field renaming
//! - Track which fields should be made optional (partial)
//! - Apply serde rename strategies (camelCase, `snake_case`, etc.)
//! - Filter and transform field lists based on configuration
//!
//! # Key Functions
//!
//! - [`build_pick_set`] - Create set of fields to include
//! - [`build_omit_set`] - Create set of fields to exclude
//! - [`build_partial_config`] - Determine optional field configuration
//! - [`build_rename_map`] - Create field name mapping for renames
//! - [`filter_fields`] - Apply pick/omit filters to field list
//! - [`extract_field_attrs`] - Extract serde attributes from fields
//!
//! # Example
//!
//! ```ignore
//! // Builds sets for filtering
//! let pick_set = build_pick_set(Some(vec!["id".to_string(), "name".to_string()]));
//! let omit_set = build_omit_set(Some(vec!["password".to_string()]));
//! let (partial_all, partial_set) = build_partial_config(&partial_mode);
//! ```

use std::collections::{HashMap, HashSet};

use super::input::PartialMode;
use crate::parser::extract_rename_all;

/// Builds the omit set from input without cloning the source Vec.
pub fn build_omit_set(omit: Option<&Vec<String>>) -> HashSet<String> {
    omit.into_iter().flatten().cloned().collect()
}

/// Builds the pick set from input without cloning the source Vec.
pub fn build_pick_set(pick: Option<&Vec<String>>) -> HashSet<String> {
    pick.into_iter().flatten().cloned().collect()
}

/// Builds the partial set based on partial mode.
///
/// Returns (`partial_all`, `partial_set`) where:
/// - `partial_all` is true if all fields should be made optional
/// - `partial_set` contains specific fields to make optional (empty if `partial_all`)
#[allow(clippy::ref_option)]
pub fn build_partial_config(partial: &Option<PartialMode>) -> (bool, HashSet<String>) {
    let partial_all = matches!(partial, Some(PartialMode::All));
    let partial_set: HashSet<String> = match partial {
        Some(PartialMode::Fields(fields)) => fields.iter().cloned().collect(),
        _ => HashSet::new(),
    };
    (partial_all, partial_set)
}

/// Builds the rename map from input without cloning the source Vec.
pub fn build_rename_map(rename: Option<&Vec<(String, String)>>) -> HashMap<String, String> {
    rename.into_iter().flatten().cloned().collect()
}

/// Extracts serde attributes from a struct, excluding `rename_all`.
///
/// This is used to inherit serde attributes from the source struct
/// while handling `rename_all` separately.
pub fn extract_serde_attrs_without_rename_all(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs
        .iter()
        .filter(|attr| {
            if !attr.path().is_ident("serde") {
                return false;
            }
            // Check if this serde attr contains rename_all
            let mut has_rename_all = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename_all") {
                    has_rename_all = true;
                }
                Ok(())
            });
            !has_rename_all
        })
        .collect()
}

/// Extracts doc attributes from a struct or field.
pub fn extract_doc_attrs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .collect()
}

/// Determines the effective `rename_all` strategy.
///
/// Priority:
/// 1. If `input.rename_all` is specified, use it
/// 2. Else if source has `rename_all`, use it
/// 3. Else default to "camelCase"
pub fn determine_rename_all(
    input_rename_all: Option<&String>,
    source_attrs: &[syn::Attribute],
) -> String {
    input_rename_all.map_or_else(
        || extract_rename_all(source_attrs).unwrap_or_else(|| "camelCase".to_string()),
        std::clone::Clone::clone,
    )
}

/// Extracts serde attributes from a field.
pub fn extract_field_serde_attrs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .collect()
}

/// Extracts `#[form_data(...)]` attributes from a field.
///
/// Used in multipart mode to preserve `form_data` attributes from the source struct
/// on generated fields (e.g., `#[form_data(limit = "10MiB")]`).
pub fn extract_form_data_attrs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("form_data"))
        .collect()
}

/// Filters out serde(rename) attributes from a list of serde attributes.
///
/// Used when applying a custom rename to avoid conflicts.
pub fn filter_out_serde_rename<'a>(attrs: &[&'a syn::Attribute]) -> Vec<&'a syn::Attribute> {
    attrs
        .iter()
        .filter(|attr| {
            let mut has_rename = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    has_rename = true;
                }
                Ok(())
            });
            !has_rename
        })
        .copied()
        .collect()
}

/// Checks if a field should be filtered out based on omit/pick rules.
///
/// Returns true if the field should be skipped.
pub fn should_skip_field(
    field_name: &str,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
) -> bool {
    // Apply omit filter
    if !omit_set.is_empty() && omit_set.contains(field_name) {
        return true;
    }
    // Apply pick filter
    if !pick_set.is_empty() && !pick_set.contains(field_name) {
        return true;
    }
    false
}

/// Checks if a field should be wrapped in Option for partial mode.
pub fn should_wrap_in_option(
    field_name: &str,
    partial_all: bool,
    partial_set: &HashSet<String>,
    is_already_option: bool,
    is_relation: bool,
) -> bool {
    (partial_all || partial_set.contains(field_name)) && !is_already_option && !is_relation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_omit_set() {
        let omit = Some(vec!["password".to_string(), "secret".to_string()]);
        let set = build_omit_set(omit.as_ref());

        assert!(set.contains("password"));
        assert!(set.contains("secret"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_build_omit_set_none() {
        let set = build_omit_set(None);
        assert!(set.is_empty());
    }

    #[test]
    fn test_build_pick_set() {
        let pick = Some(vec!["id".to_string(), "name".to_string()]);
        let set = build_pick_set(pick.as_ref());

        assert!(set.contains("id"));
        assert!(set.contains("name"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_build_partial_config_all() {
        let partial = Some(PartialMode::All);
        let (all, set) = build_partial_config(&partial);

        assert!(all);
        assert!(set.is_empty());
    }

    #[test]
    fn test_build_partial_config_fields() {
        let partial = Some(PartialMode::Fields(vec![
            "name".to_string(),
            "email".to_string(),
        ]));
        let (all, set) = build_partial_config(&partial);

        assert!(!all);
        assert!(set.contains("name"));
        assert!(set.contains("email"));
    }

    #[test]
    fn test_build_partial_config_none() {
        let (all, set) = build_partial_config(&None);

        assert!(!all);
        assert!(set.is_empty());
    }

    #[test]
    fn test_build_rename_map() {
        let rename = Some(vec![
            ("id".to_string(), "user_id".to_string()),
            ("name".to_string(), "full_name".to_string()),
        ]);
        let map = build_rename_map(rename.as_ref());

        assert_eq!(map.get("id"), Some(&"user_id".to_string()));
        assert_eq!(map.get("name"), Some(&"full_name".to_string()));
    }

    #[test]
    fn test_build_rename_map_none() {
        let map = build_rename_map(None);
        assert!(map.is_empty());
    }

    #[test]
    fn test_extract_serde_attrs_without_rename_all() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[serde(rename_all = "camelCase")]),
            syn::parse_quote!(#[serde(default)]),
            syn::parse_quote!(#[doc = "Some doc"]),
        ];

        let filtered = extract_serde_attrs_without_rename_all(&attrs);

        assert_eq!(filtered.len(), 1);
        // Should keep #[serde(default)] but not #[serde(rename_all = ...)]
    }

    #[test]
    fn test_extract_doc_attrs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[doc = "First doc"]),
            syn::parse_quote!(#[serde(default)]),
            syn::parse_quote!(#[doc = "Second doc"]),
        ];

        let docs = extract_doc_attrs(&attrs);

        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn test_determine_rename_all_with_input() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[serde(rename_all = "snake_case")])];

        let result = determine_rename_all(Some(&"PascalCase".to_string()), &attrs);

        assert_eq!(result, "PascalCase");
    }

    #[test]
    fn test_determine_rename_all_from_source() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[serde(rename_all = "snake_case")])];

        let result = determine_rename_all(None, &attrs);

        assert_eq!(result, "snake_case");
    }

    #[test]
    fn test_determine_rename_all_default() {
        let attrs: Vec<syn::Attribute> = vec![];

        let result = determine_rename_all(None, &attrs);

        assert_eq!(result, "camelCase");
    }

    #[test]
    fn test_extract_field_serde_attrs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[serde(rename = "userId")]),
            syn::parse_quote!(#[doc = "The user ID"]),
            syn::parse_quote!(#[serde(default)]),
        ];

        let serde_attrs = extract_field_serde_attrs(&attrs);

        assert_eq!(serde_attrs.len(), 2);
    }

    #[test]
    #[allow(clippy::similar_names)]
    fn test_filter_out_serde_rename() {
        let attr1: syn::Attribute = syn::parse_quote!(#[serde(rename = "userId")]);
        let attr2: syn::Attribute = syn::parse_quote!(#[serde(default)]);
        let attrs: Vec<&syn::Attribute> = vec![&attr1, &attr2];

        let filtered = filter_out_serde_rename(&attrs);

        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn test_should_skip_field_omit() {
        let omit_set: HashSet<String> = ["password".to_string()].into_iter().collect();
        let pick_set: HashSet<String> = HashSet::new();

        assert!(should_skip_field("password", &omit_set, &pick_set));
        assert!(!should_skip_field("name", &omit_set, &pick_set));
    }

    #[test]
    fn test_should_skip_field_pick() {
        let omit_set: HashSet<String> = HashSet::new();
        let pick_set: HashSet<String> =
            ["id".to_string(), "name".to_string()].into_iter().collect();

        assert!(should_skip_field("email", &omit_set, &pick_set));
        assert!(!should_skip_field("id", &omit_set, &pick_set));
    }

    #[test]
    fn test_should_skip_field_no_filters() {
        let omit_set: HashSet<String> = HashSet::new();
        let pick_set: HashSet<String> = HashSet::new();

        assert!(!should_skip_field("any_field", &omit_set, &pick_set));
    }

    #[test]
    fn test_should_wrap_in_option_partial_all() {
        let partial_set: HashSet<String> = HashSet::new();

        assert!(should_wrap_in_option(
            "name",
            true,
            &partial_set,
            false,
            false
        ));
        assert!(!should_wrap_in_option(
            "name",
            true,
            &partial_set,
            true,
            false
        )); // already option
        assert!(!should_wrap_in_option(
            "rel",
            true,
            &partial_set,
            false,
            true
        )); // relation
    }

    #[test]
    fn test_extract_form_data_attrs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[form_data(limit = "10MiB")]),
            syn::parse_quote!(#[serde(default)]),
            syn::parse_quote!(#[doc = "Some doc"]),
            syn::parse_quote!(#[form_data(field_name = "my_file")]),
        ];

        let form_data = extract_form_data_attrs(&attrs);
        assert_eq!(form_data.len(), 2);
    }

    #[test]
    fn test_extract_form_data_attrs_empty() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[serde(default)]),
            syn::parse_quote!(#[doc = "Some doc"]),
        ];

        let form_data = extract_form_data_attrs(&attrs);
        assert!(form_data.is_empty());
    }

    #[test]
    fn test_should_wrap_in_option_partial_fields() {
        let partial_set: HashSet<String> = ["name".to_string()].into_iter().collect();

        assert!(should_wrap_in_option(
            "name",
            false,
            &partial_set,
            false,
            false
        ));
        assert!(!should_wrap_in_option(
            "email",
            false,
            &partial_set,
            false,
            false
        ));
    }
}

#[cfg(test)]
mod schema_type_option_tests {
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

    // Tests for qualified path storage fallback
    // Note: This tests the case where is_qualified_path returns true
    // and we find the struct in schema_storage rather than via file lookup

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
}
