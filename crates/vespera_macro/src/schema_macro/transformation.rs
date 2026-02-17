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

/// Builds the omit set from input.
pub fn build_omit_set(omit: Option<Vec<String>>) -> HashSet<String> {
    omit.unwrap_or_default().into_iter().collect()
}

/// Builds the pick set from input.
pub fn build_pick_set(pick: Option<Vec<String>>) -> HashSet<String> {
    pick.unwrap_or_default().into_iter().collect()
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

/// Builds the rename map from input.
pub fn build_rename_map(rename: Option<Vec<(String, String)>>) -> HashMap<String, String> {
    rename.unwrap_or_default().into_iter().collect()
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
        let set = build_omit_set(omit);

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
        let set = build_pick_set(pick);

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
        let map = build_rename_map(rename);

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
