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

use std::collections::{BTreeSet, HashSet};

fn sorted_source_fields(source_field_names: &HashSet<String>) -> Vec<&str> {
    source_field_names
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn validate_fields_exist<'a>(
    kind: &str,
    fields: impl IntoIterator<Item = &'a str>,
    source_field_names: &HashSet<String>,
    source_type: &syn::Type,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    for field in fields {
        if !source_field_names.contains(field) {
            let prefix = if kind == "partial" {
                "partial field"
            } else {
                "field"
            };
            return Err(syn::Error::new_spanned(
                source_type,
                format!(
                    "{prefix} `{field}` does not exist in type `{source_type_name}`. Available fields: {:?}",
                    sorted_source_fields(source_field_names)
                ),
            ));
        }
    }
    Ok(())
}

/// Validates that all fields in `pick` exist in the source struct.
///
/// Returns an error if any field in `pick` does not exist.
pub fn validate_pick_fields(
    pick_fields: Option<&Vec<String>>,
    source_field_names: &HashSet<String>,
    source_type: &syn::Type,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    validate_fields_exist(
        "pick",
        pick_fields.into_iter().flatten().map(String::as_str),
        source_field_names,
        source_type,
        source_type_name,
    )
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
    validate_fields_exist(
        "omit",
        omit_fields.into_iter().flatten().map(String::as_str),
        source_field_names,
        source_type,
        source_type_name,
    )
}

/// Returns `true` when `name` is a legal Rust identifier — i.e. the
/// downstream `syn::Ident::new(name, ..)` that turns a `rename`/`add`
/// target into a struct field identifier cannot panic on it.
///
/// `syn::parse_str::<Ident>` rejects non-identifiers (`"user-id"`,
/// `"a b"`, `""`, a leading digit) AND reserved keywords (`"type"`,
/// `"match"`) — both of which would otherwise either panic
/// `Ident::new` or emit a struct field that fails to compile. Raw
/// identifiers (`"r#type"`) are accepted.
fn is_valid_field_ident(name: &str) -> bool {
    syn::parse_str::<syn::Ident>(name).is_ok()
}

/// Validates a `rename` pair list: every **source** field must exist in
/// the source struct, and every **target** name must be a legal Rust
/// identifier.
///
/// The target check is what stops a `schema_type!(.., rename = [("id",
/// "user-id")])` (or a keyword target like `"type"`) from panicking the
/// proc-macro at `syn::Ident::new` — it now surfaces as a spanned compile
/// error instead of an opaque expansion abort.
pub fn validate_rename_fields(
    rename_pairs: Option<&Vec<(String, String)>>,
    source_field_names: &HashSet<String>,
    source_type: &syn::Type,
    source_type_name: &str,
) -> Result<(), syn::Error> {
    validate_fields_exist(
        "rename",
        rename_pairs
            .into_iter()
            .flatten()
            .map(|(from_field, _)| from_field.as_str()),
        source_field_names,
        source_type,
        source_type_name,
    )?;
    for (from_field, to_field) in rename_pairs.into_iter().flatten() {
        if !is_valid_field_ident(to_field) {
            return Err(syn::Error::new_spanned(
                source_type,
                format!(
                    "rename target `{to_field}` (for source field `{from_field}`) is not a valid \
                     Rust identifier; use letters/digits/`_` (not starting with a digit) and avoid \
                     reserved keywords"
                ),
            ));
        }
    }
    Ok(())
}

/// Validates that every `add = [(name: Type)]` field name is a legal Rust
/// identifier, so the `syn::Ident::new(name, ..)` that materializes the
/// added field cannot panic on a non-identifier / keyword name (same
/// class of bug as an invalid `rename` target).
pub fn validate_add_field_idents(
    add: Option<&Vec<(String, syn::Type)>>,
    source_type: &syn::Type,
) -> Result<(), syn::Error> {
    for (name, _) in add.into_iter().flatten() {
        if !is_valid_field_ident(name) {
            return Err(syn::Error::new_spanned(
                source_type,
                format!(
                    "`add` field name `{name}` is not a valid Rust identifier; use \
                     letters/digits/`_` (not starting with a digit) and avoid reserved keywords"
                ),
            ));
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
    validate_fields_exist(
        "partial",
        partial_fields.into_iter().flatten().map(String::as_str),
        source_field_names,
        source_type,
        source_type_name,
    )
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
    fn test_validate_rename_fields_invalid_target_ident() {
        // Renaming to a non-identifier ("user-id") must surface as a spanned
        // error, NOT panic the proc-macro at the downstream `syn::Ident::new`.
        let source_fields = create_field_names(&["id", "name"]);
        let rename = Some(vec![("id".to_string(), "user-id".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not a valid"));
        assert!(err.contains("user-id"));
    }

    #[test]
    fn test_validate_rename_fields_keyword_target_rejected() {
        // A reserved keyword target ("type") would emit an uncompilable field
        // and `syn::Ident::new` rejects it — surface a clean error instead.
        let source_fields = create_field_names(&["id"]);
        let rename = Some(vec![("id".to_string(), "type".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_rename_fields_raw_ident_target_ok() {
        // A raw identifier target (`r#type`) is a legal field name and must pass.
        let source_fields = create_field_names(&["id"]);
        let rename = Some(vec![("id".to_string(), "r#type".to_string())]);
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();

        let result = validate_rename_fields(rename.as_ref(), &source_fields, &ty, "User");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_add_field_idents_valid() {
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let add = Some(vec![
            ("extra".to_string(), syn::parse_quote!(String)),
            ("count".to_string(), syn::parse_quote!(i32)),
        ]);
        assert!(validate_add_field_idents(add.as_ref(), &ty).is_ok());
    }

    #[test]
    fn test_validate_add_field_idents_invalid() {
        // An `add` name that is not a valid identifier must error, not panic.
        let ty: syn::Type = syn::parse2(quote!(User)).unwrap();
        let add = Some(vec![("bad-name".to_string(), syn::parse_quote!(String))]);
        let result = validate_add_field_idents(add.as_ref(), &ty);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("bad-name"));
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
