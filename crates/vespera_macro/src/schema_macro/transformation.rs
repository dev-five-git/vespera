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
//! - [`container_serde_attrs`] / [`field_serde_attrs`] - Rebuild serde attributes for the generated struct
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

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;

use super::input::PartialMode;
use crate::parser::extract_rename_all;
use crate::parser::schema::{SerdeSide, extract_field_rename_for, rename_field};

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

/// Container serde keys that are never copied onto the generated struct:
/// `rename_all` is re-emitted from the effective strategy, and `from` /
/// `try_from` / `into` / `default` require impls (`From`, `TryFrom`, `Into`,
/// `Default`) the generated type does not have.
const CONTAINER_STRIPPED_KEYS: &[&str] = &["rename_all", "from", "try_from", "into", "default"];

/// Name of a serde list item (`rename`, `default`, `with`, ...).
fn serde_key(meta: &syn::Meta) -> String {
    meta.path()
        .segments
        .last()
        .map(|seg| seg.ident.to_string())
        .unwrap_or_default()
}

/// Rebuilds `#[serde(...)]` attributes item by item, keeping only the items
/// `keep` accepts. An attribute left without items is dropped; one whose list
/// does not parse as `syn::Meta` items is kept verbatim so serde itself
/// reports the problem.
pub fn rebuild_serde_attrs<'a>(
    attrs: impl IntoIterator<Item = &'a syn::Attribute>,
    mut keep: impl FnMut(&syn::Meta) -> bool,
) -> Vec<TokenStream> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let Ok(items) =
            attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        else {
            out.push(quote! { #attr });
            continue;
        };
        let kept: Vec<&syn::Meta> = items.iter().filter(|meta| keep(meta)).collect();
        if !kept.is_empty() {
            out.push(quote! { #[serde(#(#kept),*)] });
        }
    }
    out
}

/// Container serde attributes for the generated struct: every source key
/// except [`CONTAINER_STRIPPED_KEYS`], rebuilt key by key so e.g.
/// `deny_unknown_fields` survives next to a stripped `rename_all`.
pub fn container_serde_attrs(attrs: &[syn::Attribute]) -> Vec<TokenStream> {
    rebuild_serde_attrs(attrs, |meta| {
        !CONTAINER_STRIPPED_KEYS.contains(&serde_key(meta).as_str())
    })
}

/// Field serde attributes for the generated struct.
///
/// - `strip_rename`: drop `rename` (the caller emits the wire name itself).
/// - `wrapped_in_option`: the field type became `Option<T>` (`partial`), so
///   the keys typed against `T` — `default = "path"`, `with`,
///   `serialize_with`, `deserialize_with`, `skip_serializing_if` — are
///   dropped (a missing `Option` field already deserializes to `None`). A
///   dropped `skip_serializing_if` becomes `skip_serializing_if =
///   "Option::is_none"`, so an unset field stays absent from the output.
pub fn field_serde_attrs(
    attrs: &[syn::Attribute],
    wrapped_in_option: bool,
    strip_rename: bool,
) -> Vec<TokenStream> {
    let mut dropped_skip_if = false;
    let mut out = rebuild_serde_attrs(attrs, |meta| {
        let key = serde_key(meta);
        if strip_rename && key == "rename" {
            return false;
        }
        if !wrapped_in_option {
            return true;
        }
        match key.as_str() {
            "skip_serializing_if" => {
                dropped_skip_if = true;
                false
            }
            "with" | "serialize_with" | "deserialize_with" => false,
            "default" => matches!(meta, syn::Meta::Path(_)),
            _ => true,
        }
    });
    if dropped_skip_if {
        out.push(quote! { #[serde(skip_serializing_if = "Option::is_none")] });
    }
    out
}

/// Wire name of a field the `rename` parameter renamed: the Rust field
/// changes, the JSON key does not. It is the source field's explicit serde
/// `rename` (per side), else `rename_all` applied to the SOURCE field name.
pub fn renamed_field_serde_rename(
    attrs: &[syn::Attribute],
    source_field_name: &str,
    rename_all: &str,
) -> TokenStream {
    let serde_only: Vec<syn::Attribute> = attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .cloned()
        .collect();
    let fallback = || rename_field(source_field_name, Some(rename_all));
    let ser = extract_field_rename_for(&serde_only, SerdeSide::Serialize).unwrap_or_else(fallback);
    let de = extract_field_rename_for(&serde_only, SerdeSide::Deserialize).unwrap_or_else(fallback);
    if ser == de {
        quote! { #[serde(rename = #ser)] }
    } else {
        quote! { #[serde(rename(serialize = #ser, deserialize = #de))] }
    }
}

/// Multipart twin of [`renamed_field_serde_rename`]: pins the form field name
/// of a renamed field to the source's (`rename_all` applied to the SOURCE
/// field name) with `#[form_data(field_name = "...")]`. `None` when the
/// source already names it via `#[form_data(field_name)]` (copied as is).
pub fn renamed_field_form_name(
    attrs: &[syn::Attribute],
    source_field_name: &str,
    rename_all: &str,
) -> Option<TokenStream> {
    let form_data_only: Vec<syn::Attribute> = attrs
        .iter()
        .filter(|attr| attr.path().is_ident("form_data"))
        .cloned()
        .collect();
    if extract_field_rename_for(&form_data_only, SerdeSide::Serialize).is_some() {
        return None;
    }
    let name = rename_field(source_field_name, Some(rename_all));
    Some(quote! { #[form_data(field_name = #name)] })
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
