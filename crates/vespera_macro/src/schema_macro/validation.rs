//! Field validation logic for `schema_type`! macro.
//!
//! This module contains functions to validate that fields specified in
//! pick, omit, rename, and partial parameters exist in the source struct.
//!
//! # Overview
//!
//! The `schema_type`! macro accepts user-specified field filters (pick, omit, rename, partial).
//! This module validates that all specified fields actually exist in the source struct,
//! providing clear error messages when fields don't exist.
//!
//! # Validation Functions
//!
//! - [`validate_pick_fields`] - Ensure all pick fields exist
//! - [`validate_omit_fields`] - Ensure all omit fields exist
//! - [`validate_rename_fields`] - Ensure all rename source fields exist
//! - [`validate_partial_fields`] - Ensure all partial fields exist
//! - [`extract_source_field_names`] - Extract all field names from a struct
//!
//! # Example
//!
//! ```ignore
//! // This validates that "user_id", "name" exist in Model
//! schema_type!(UserResponse from Model, pick = ["user_id", "name"]);
//!
//! // If "nonexistent" doesn't exist, validation error is raised at compile time
//! schema_type!(BadSchema from Model, pick = ["nonexistent"]);
//! ```

use std::collections::HashSet;

/// Validates that all fields in `pick` exist in the source struct.
///
/// Returns an error if any field in `pick` does not exist.
pub fn validate_pick_fields(
    pick_fields: Option<&Vec<String>>,
    source_field_names: &HashSet<String>,
    source_type: &syn::Type,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    if let Some(fields) = pick_fields {
        for field in fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    source_type,
                    format!(
                        "field `{}` does not exist in type `{}`. Available fields: {:?}",
                        field,
                        source_type_name,
                        source_field_names.iter().collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Validates that all fields in `omit` exist in the source struct.
///
/// Returns an error if any field in `omit` does not exist.
pub fn validate_omit_fields(
    omit_fields: Option<&Vec<String>>,
    source_field_names: &HashSet<String>,
    source_type: &syn::Type,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    if let Some(fields) = omit_fields {
        for field in fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    source_type,
                    format!(
                        "field `{}` does not exist in type `{}`. Available fields: {:?}",
                        field,
                        source_type_name,
                        source_field_names.iter().collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Validates that all source fields in `rename` exist in the source struct.
///
/// Returns an error if any source field in a rename pair does not exist.
pub fn validate_rename_fields(
    rename_pairs: Option<&Vec<(String, String)>>,
    source_field_names: &HashSet<String>,
    source_type: &syn::Type,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    if let Some(pairs) = rename_pairs {
        for (from_field, _) in pairs {
            if !source_field_names.contains(from_field) {
                return Err(syn::Error::new_spanned(
                    source_type,
                    format!(
                        "field `{}` does not exist in type `{}`. Available fields: {:?}",
                        from_field,
                        source_type_name,
                        source_field_names.iter().collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Validates that all fields in `partial` (when specific fields are listed) exist in the source struct.
///
/// Returns an error if any field in `partial` does not exist.
pub fn validate_partial_fields(
    partial_fields: Option<&Vec<String>>,
    source_field_names: &HashSet<String>,
    source_type: &syn::Type,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    if let Some(fields) = partial_fields {
        for field in fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    source_type,
                    format!(
                        "partial field `{}` does not exist in type `{}`. Available fields: {:?}",
                        field,
                        source_type_name,
                        source_field_names.iter().collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Extracts all field names from a struct's named fields.
///
/// Returns an empty set for tuple or unit structs.
pub fn extract_source_field_names(parsed_struct: &syn::ItemStruct) -> HashSet<String> {
    use crate::parser::strip_raw_prefix_owned;

    if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
        fields_named
            .named
            .iter()
            .filter_map(|f| f.ident.as_ref())
            .map(|i| strip_raw_prefix_owned(i.to_string()))
            .collect()
    } else {
        HashSet::new()
    }
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    fn create_field_names(names: &[&str]) -> HashSet<String> {
        names.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn test_validate_pick_fields_success() {
        let source_fields = create_field_names(&["id", "name", "email"]);
        let pick = Some(vec!["id".to_string(), "name".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_pick_fields(pick.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_pick_fields_nonexistent() {
        let source_fields = create_field_names(&["id", "name"]);
        let pick = Some(vec!["nonexistent".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_pick_fields(pick.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_validate_pick_fields_none() {
        let source_fields = create_field_names(&["id", "name"]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_pick_fields(None, &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_omit_fields_success() {
        let source_fields = create_field_names(&["id", "name", "password"]);
        let omit = Some(vec!["password".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_omit_fields(omit.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_omit_fields_nonexistent() {
        let source_fields = create_field_names(&["id", "name"]);
        let omit = Some(vec!["missing".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_omit_fields(omit.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn test_validate_rename_fields_success() {
        let source_fields = create_field_names(&["id", "name"]);
        let rename = Some(vec![("id".to_string(), "user_id".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_rename_fields_nonexistent() {
        let source_fields = create_field_names(&["id", "name"]);
        let rename = Some(vec![("missing".to_string(), "new_name".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn test_validate_partial_fields_success() {
        let source_fields = create_field_names(&["id", "name", "email"]);
        let partial = Some(vec!["name".to_string(), "email".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_partial_fields(partial.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_partial_fields_nonexistent() {
        let source_fields = create_field_names(&["id", "name"]);
        let partial = Some(vec!["nonexistent".to_string()]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_partial_fields(partial.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn test_extract_source_field_names_named() {
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct User { pub id: i32, pub name: String }").unwrap();
        let names = extract_source_field_names(&struct_def);

        assert!(names.contains("id"));
        assert!(names.contains("name"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn test_extract_source_field_names_tuple() {
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct Point(pub i32, pub i32);").unwrap();
        let names = extract_source_field_names(&struct_def);

        assert!(names.is_empty());
    }

    #[test]
    fn test_extract_source_field_names_raw_identifier() {
        let struct_def: syn::ItemStruct =
            syn::parse_str("pub struct Config { pub r#type: String }").unwrap();
        let names = extract_source_field_names(&struct_def);

        assert!(names.contains("type"));
        assert_eq!(names.len(), 1);
    }
}
