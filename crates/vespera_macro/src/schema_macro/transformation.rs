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
mod tests;

#[cfg(test)]
mod schema_type_option_tests;
