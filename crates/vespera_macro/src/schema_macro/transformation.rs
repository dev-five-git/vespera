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
use super::module_scope::{Resolved, resolve_name};
use crate::parser::schema::{SerdeSide, extract_field_rename_for, rename_field};
use crate::parser::{extract_rename_all, extract_rename_all_for};

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
///
/// A struct `#[serde(tag = "...")]` writes the container's serde name as the
/// tag value, which defaults to the Rust ident. When the source has a `tag`
/// but no container `rename`, the DTO gets `#[serde(rename = "<Source>")]`
/// so the tag value stays the source's (a source `rename` is copied as is).
pub fn container_serde_attrs(attrs: &[syn::Attribute], source_ident: &str) -> Vec<TokenStream> {
    let mut has_tag = false;
    let mut has_rename = false;
    let mut out = rebuild_serde_attrs(attrs, |meta| {
        let key = serde_key(meta);
        has_tag |= key == "tag";
        has_rename |= key == "rename";
        !CONTAINER_STRIPPED_KEYS.contains(&key.as_str())
    });
    if has_tag && !has_rename {
        out.push(quote! { #[serde(rename = #source_ident)] });
    }
    out
}

/// A field `partial` wrapped into `Option<T>`: the context needed to keep a
/// custom serde codec (`with` / `serialize_with` / `deserialize_with`) on it.
pub struct OptionWrap<'a> {
    /// Prefix of the generated adapter fns (`__vespera_<Struct>_<field>`).
    pub adapter_prefix: &'a str,
    /// The unwrapped field type `T`.
    pub inner_ty: &'a TokenStream,
}

/// Field serde attributes for the generated struct, plus the adapter fns a
/// `partial` field with a custom codec needs (empty otherwise).
///
/// - `strip_rename`: drop `rename` (the caller emits the wire name itself).
/// - `wrap`: the field type became `Option<T>` (`partial`), so the keys
///   typed against `T` cannot be copied verbatim:
///   - `default = "path"` is dropped (a missing `Option` field already
///     deserializes to `None`);
///   - `skip_serializing_if` becomes `skip_serializing_if =
///     "Option::is_none"`, so an unset field stays absent from the output;
///   - `with` / `serialize_with` / `deserialize_with` are routed through
///     generated adapters over `Option<T>` (`Some` goes through the source
///     fn, `None` serializes as none), so the field keeps the source's wire
///     format. Such a field also gets `default` (serde requires it for a
///     missing field read by a custom fn) and `skip_serializing_if =
///     "Option::is_none"`.
pub fn field_serde_attrs(
    attrs: &[syn::Attribute],
    wrap: Option<&OptionWrap<'_>>,
    strip_rename: bool,
) -> (Vec<TokenStream>, TokenStream) {
    let mut dropped_skip_if = false;
    let mut kept_default = false;
    let mut ser_fn: Option<syn::ExprPath> = None;
    let mut de_fn: Option<syn::ExprPath> = None;
    let mut out = rebuild_serde_attrs(attrs, |meta| {
        let key = serde_key(meta);
        if strip_rename && key == "rename" {
            return false;
        }
        if wrap.is_none() {
            return true;
        }
        match key.as_str() {
            "skip_serializing_if" => {
                dropped_skip_if = true;
                false
            }
            "with" | "serialize_with" | "deserialize_with" => {
                if let Some(path) = codec_path(meta) {
                    match key.as_str() {
                        "with" => {
                            ser_fn = Some(append_segment(&path, "serialize"));
                            de_fn = Some(append_segment(&path, "deserialize"));
                        }
                        "serialize_with" => ser_fn = Some(path),
                        _ => de_fn = Some(path),
                    }
                }
                false
            }
            "default" => {
                let bare = matches!(meta, syn::Meta::Path(_));
                kept_default |= bare;
                bare
            }
            _ => true,
        }
    });
    let Some(wrap) = wrap else {
        return (out, TokenStream::new());
    };
    let inner = wrap.inner_ty;
    let mut keys: Vec<TokenStream> = Vec::new();
    let mut adapters = TokenStream::new();
    if de_fn.is_some() && !kept_default {
        keys.push(quote! { default });
    }
    if let Some(ser) = &ser_fn {
        let name = format!("{}_ser", wrap.adapter_prefix);
        let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
        keys.push(quote! { serialize_with = #name });
        adapters.extend(quote! {
            #[doc(hidden)]
            #[allow(non_snake_case, clippy::ref_option)]
            fn #ident<__S: serde::Serializer>(
                value: &Option<#inner>,
                serializer: __S,
            ) -> Result<__S::Ok, __S::Error> {
                match value {
                    Some(inner) => #ser(inner, serializer),
                    None => serializer.serialize_none(),
                }
            }
        });
    }
    if let Some(de) = &de_fn {
        let name = format!("{}_de", wrap.adapter_prefix);
        let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
        keys.push(quote! { deserialize_with = #name });
        adapters.extend(quote! {
            #[doc(hidden)]
            #[allow(non_snake_case)]
            fn #ident<'de, __D: serde::Deserializer<'de>>(
                deserializer: __D,
            ) -> Result<Option<#inner>, __D::Error> {
                #de(deserializer).map(Some)
            }
        });
    }
    if dropped_skip_if || ser_fn.is_some() || de_fn.is_some() {
        keys.push(quote! { skip_serializing_if = "Option::is_none" });
    }
    if !keys.is_empty() {
        out.push(quote! { #[serde(#(#keys),*)] });
    }
    (out, adapters)
}

/// Serde keys whose string value is a function / module path.
const SERDE_PATH_KEYS: &[&str] = &[
    "default",
    "skip_serializing_if",
    "with",
    "serialize_with",
    "deserialize_with",
];

/// The source field's attributes with every serde function path
/// (`default = "path"`, `skip_serializing_if`, `with`, `serialize_with`,
/// `deserialize_with`) re-rooted at the SOURCE module: the DTO is emitted
/// at the `schema_type!` call site, where `default_name` /
/// `codecs::as_string` of another module did not resolve. Paths the source
/// module does not define or import (`Option::is_none`, `String::is_empty`)
/// are kept as written.
pub fn reroot_serde_paths(attrs: &[syn::Attribute], module_path: &[String]) -> Vec<syn::Attribute> {
    attrs
        .iter()
        .map(|attr| {
            if !attr.path().is_ident("serde") {
                return attr.clone();
            }
            let Ok(mut items) =
                attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
            else {
                return attr.clone();
            };
            for meta in &mut items {
                if let syn::Meta::NameValue(nv) = meta
                    && SERDE_PATH_KEYS.contains(&serde_key_of(&nv.path).as_str())
                    && let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit),
                        ..
                    }) = &mut nv.value
                    && let Some(rerooted) = reroot_path(&lit.value(), module_path)
                {
                    *lit = syn::LitStr::new(&rerooted, lit.span());
                }
            }
            syn::parse_quote! { #[serde(#items)] }
        })
        .collect()
}

fn serde_key_of(path: &syn::Path) -> String {
    path.get_ident()
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// `path` resolved in `module_path` (`None` when it is not a path or not
/// known there).
fn reroot_path(path: &str, module_path: &[String]) -> Option<String> {
    let path: syn::Path = syn::parse_str(path).ok()?;
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    match resolve_name(&segments, module_path) {
        Resolved::Path(resolved) if path.leading_colon.is_none() => Some(resolved.join("::")),
        _ => None,
    }
}

/// The function / module path of a `with = "..."`-style key (`None` when it

/// is not a string path; the key is then dropped).
fn codec_path(meta: &syn::Meta) -> Option<syn::ExprPath> {
    let syn::Meta::NameValue(nv) = meta else {
        return None;
    };
    let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(lit),
        ..
    }) = &nv.value
    else {
        return None;
    };
    lit.parse().ok()
}

/// `module` + `serialize` -> `module::serialize` (serde's `with` expansion).
fn append_segment(path: &syn::ExprPath, segment: &str) -> syn::ExprPath {
    let mut path = path.clone();
    path.path
        .segments
        .push(syn::PathSegment::from(syn::Ident::new(
            segment,
            proc_macro2::Span::call_site(),
        )));
    path
}

/// Wire name of a field the `rename` parameter renamed: the Rust field
/// changes, the JSON key does not. It is the source field's explicit serde
/// `rename` (per side), else `rename_all` applied to the SOURCE field name.
pub fn renamed_field_serde_rename(
    attrs: &[syn::Attribute],
    source_field_name: &str,
    rename_all: &RenameAll,
) -> TokenStream {
    let serde_only: Vec<syn::Attribute> = attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .cloned()
        .collect();
    let ser = extract_field_rename_for(&serde_only, SerdeSide::Serialize)
        .unwrap_or_else(|| rename_field(source_field_name, rename_all.serialize.as_deref()));
    let de = extract_field_rename_for(&serde_only, SerdeSide::Deserialize)
        .unwrap_or_else(|| rename_field(source_field_name, rename_all.deserialize.as_deref()));
    if ser == de {
        quote! { #[serde(rename = #ser)] }
    } else {
        quote! { #[serde(rename(serialize = #ser, deserialize = #de))] }
    }
}

/// Multipart twin of [`renamed_field_serde_rename`]: pins the form field name
/// of a generated field to the source's with `#[form_data(field_name =
/// "...")]`, resolved in the `Multipart` derive's order:
///
/// 1. the source's `#[form_data(field_name)]` -> `None` (copied as is);
/// 2. the source's deserialize-side `#[serde(rename)]` (multipart mode does not copy serde
///    attributes, so it is pinned for renamed AND kept fields);
/// 3. `renamed` only: `rename_all` applied to the SOURCE field name (the
///    Rust field changed, the form field must not).
pub fn field_form_name(
    attrs: &[syn::Attribute],
    source_field_name: &str,
    rename_all: &str,
    renamed: bool,
) -> Option<TokenStream> {
    let only = |name: &str| -> Vec<syn::Attribute> {
        attrs
            .iter()
            .filter(|attr| attr.path().is_ident(name))
            .cloned()
            .collect()
    };
    if extract_field_rename_for(&only("form_data"), SerdeSide::Serialize).is_some() {
        return None;
    }
    let name = match extract_field_rename_for(&only("serde"), SerdeSide::Deserialize) {
        Some(name) => name,
        None if renamed => rename_field(source_field_name, Some(rename_all)),
        None => return None,
    };
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
/// The DTO's `rename_all` per serde side. A source with
/// `#[serde(rename_all(serialize = "..", deserialize = ".."))]` keeps both
/// sides (it used to collapse to the serialize side); `None` = no renaming.
#[derive(Debug, PartialEq, Eq)]
pub struct RenameAll {
    pub serialize: Option<String>,
    pub deserialize: Option<String>,
}

impl RenameAll {
    /// The sides of the source container, unless `rename_all = ".."` was
    /// given to `schema_type!` (or the source has no per-side form): then
    /// `effective` on both sides.
    pub fn new(input: Option<&String>, source_attrs: &[syn::Attribute], effective: &str) -> Self {
        let serialize = extract_rename_all_for(source_attrs, SerdeSide::Serialize);
        let deserialize = extract_rename_all_for(source_attrs, SerdeSide::Deserialize);
        if input.is_none() && serialize != deserialize {
            Self {
                serialize,
                deserialize,
            }
        } else {
            Self {
                serialize: Some(effective.to_string()),
                deserialize: Some(effective.to_string()),
            }
        }
    }

    /// The container attribute: `rename_all = ".."`, or the per-side list.
    pub fn attr(&self) -> TokenStream {
        match (&self.serialize, &self.deserialize) {
            (Some(ser), Some(de)) if ser == de => quote! { #[serde(rename_all = #ser)] },
            (ser, de) => {
                let ser = ser.iter().map(|ser| quote! { serialize = #ser });
                let de = de.iter().map(|de| quote! { deserialize = #de });
                quote! { #[serde(rename_all(#(#ser,)* #(#de),*))] }
            }
        }
    }
}

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
