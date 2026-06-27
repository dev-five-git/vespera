//! Schema derive macro implementation.
//!
//! This module implements the `#[derive(Schema)]` derive macro that registers
//! types for `OpenAPI` schema generation.
//!
//! # Overview
//!
//! The `#[derive(Schema)]` macro registers a struct or enum for inclusion in the `OpenAPI` spec.
//! It stores metadata about the type which is later used by the `vespera!` macro to generate
//! the `OpenAPI` components/schemas section.
//!
//! # Global Schema Storage
//!
//! This module uses a global [`SCHEMA_STORAGE`] mutex to collect all schema types across
//! a crate at compile time. This is necessary because proc-macros are invoked independently,
//! so we need a shared location to gather all types before generating the final `OpenAPI` spec.
//!
//! # Custom Schema Names
//!
//! By default, the `OpenAPI` schema name matches the struct name. You can customize it:
//!
//! ```ignore
//! #[derive(Schema)]
//! #[schema(name = "CustomSchemaName")]
//! pub struct MyType { ... }
//! ```
//!
//! # Key Functions
//!
//! - [`extract_schema_name_attr`] - Extract custom name from `#[schema]` attribute
//! - [`process_derive_schema`] - Process the derive macro input and register the type

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

use crate::metadata::StructMetadata;
use crate::parser::{extract_default, strip_raw_prefix_owned};
use crate::schema_macro::file_cache::{FileFingerprint, get_file_fingerprint};

/// Per-crate registry of `#[derive(Schema)]` metadata.
///
/// The OUTER key is [`current_crate_key`] (the consuming crate's
/// `CARGO_MANIFEST_DIR`); the inner map is `schema name -> metadata` exactly
/// as before. Scoping by crate stops a long-lived rust-analyzer proc-macro
/// server — which expands MANY crates in ONE process — from leaking crate
/// A's schemas into crate B's generated `openapi.json`. A plain `cargo build`
/// runs each crate in its own process, so the outer map only ever holds one
/// bucket there; the scoping matters only for the shared-server (IDE) case.
type SchemaBucket = Arc<HashMap<String, StructMetadata>>;
type SchemaStorage = HashMap<String, SchemaBucket>;

pub static SCHEMA_STORAGE: LazyLock<Mutex<SchemaStorage>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static DEFAULT_FUNCTION_CACHE: LazyLock<Mutex<HashMap<PathBuf, DefaultFunctionCacheEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Crate-identity key for the process-global metadata registries
/// ([`SCHEMA_STORAGE`], `ROUTE_STORAGE`, `CRON_STORAGE`).
///
/// Uses `CARGO_MANIFEST_DIR` (set per-crate by cargo, and re-set per expanded
/// crate by the rust-analyzer proc-macro server). When unset — a non-cargo
/// invocation — all entries share one empty-string bucket, i.e. the prior
/// un-scoped global behaviour, which is correct for that single-build case.
#[must_use]
pub fn current_crate_key() -> String {
    crate::schema_macro::file_cache::get_manifest_dir().unwrap_or_default()
}

/// Register a `#[derive(Schema)]` metadata entry for the current crate.
///
/// Returns `Err(())` when a DIFFERENT source item is already registered under
/// `name` for THIS crate (the silent duplicate-schema-name footgun) so the
/// caller can raise a spanned compile error. Re-registration from the same
/// source identity replaces the previous metadata, which keeps long-lived
/// proc-macro servers correct across IDE edits.
pub fn register_schema(name: String, metadata: StructMetadata) -> Result<(), ()> {
    let mut guard = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let bucket = Arc::make_mut(
        guard
            .entry(current_crate_key())
            .or_insert_with(|| Arc::new(HashMap::new())),
    );
    if let Some(existing) = bucket.get(&name) {
        if existing.definition == metadata.definition
            || (existing.source_identity.is_some()
                && existing.source_identity == metadata.source_identity)
        {
            bucket.insert(name, metadata);
            return Ok(());
        }
        return Err(());
    }
    bucket.insert(name, metadata);
    Ok(())
}

fn derive_source_identity(
    input: &syn::DeriveInput,
    call_site_file: Option<&Path>,
) -> Option<String> {
    call_site_file.map(|path| format!("{}::{}", path.display(), input.ident))
}

/// Overwrite-insert a schema for the current crate — the
/// `schema_type!(.., ignore)` pre-registration path, which has no
/// duplicate-name semantics.
pub fn insert_schema(name: String, metadata: StructMetadata) {
    let mut guard = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::make_mut(
        guard
            .entry(current_crate_key())
            .or_insert_with(|| Arc::new(HashMap::new())),
    )
    .insert(name, metadata);
}

/// Snapshot of the current crate's registered schemas — a cheap `Arc` clone of
/// this crate's bucket, so consumers never deep-clone every stored definition.
#[must_use]
pub fn current_crate_schemas() -> Arc<HashMap<String, StructMetadata>> {
    SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&current_crate_key())
        .cloned()
        .unwrap_or_else(|| Arc::new(HashMap::new()))
}

#[derive(Default)]
struct SchemaAttributeSummary {
    name: Option<String>,
    has_ref_override: bool,
}

fn collect_schema_attribute_summary(attrs: &[syn::Attribute]) -> SchemaAttributeSummary {
    let mut summary = SchemaAttributeSummary::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("schema")) {
        let mut attr_name = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value = meta.value()?;
                let lit: syn::LitStr = value.parse()?;
                attr_name = Some(lit.value());
            } else {
                if meta.path.is_ident("ref") {
                    summary.has_ref_override = true;
                }
                if let Ok(value) = meta.value() {
                    let _ = value.parse::<syn::Lit>();
                }
            }
            Ok(())
        });
        if summary.name.is_none() {
            summary.name = attr_name;
        }
        if summary.name.is_some() && summary.has_ref_override {
            break;
        }
    }
    summary
}

#[derive(Clone)]
struct DefaultFunctionCacheEntry {
    fingerprint: FileFingerprint,
    /// `Arc` so a cache hit hands back a single pointer-clone instead of
    /// deep-cloning the whole `field -> default JSON` map on every derive that
    /// shares a file (the previous `BTreeMap` clone copied every entry).
    values: Arc<BTreeMap<String, serde_json::Value>>,
}

/// Extract custom schema name from #[schema(name = "...")] attribute
pub fn extract_schema_name_attr(attrs: &[syn::Attribute]) -> Option<String> {
    collect_schema_attribute_summary(attrs).name
}

/// Process derive input and return metadata + expanded code
pub fn process_derive_schema(
    input: &syn::DeriveInput,
) -> (Option<StructMetadata>, proc_macro2::TokenStream) {
    let name = &input.ident;

    // Parse every field's `#[schema(...)]` constraints ONCE here and thread the
    // resulting slice into the supplement emitter (and, behind the `validation`
    // feature gate, into garde's `Validate` emitter). The previous codepath
    // re-ran `try_extract_schema_constraints` per field inside each callee — two
    // walks by default, three with `validation` on — for byte-identical output.
    let mut field_constraints: Vec<crate::parser::schema::schema_attrs::SchemaConstraints> =
        Vec::new();
    if let syn::Data::Struct(data_struct) = &input.data
        && let syn::Fields::Named(fields_named) = &data_struct.fields
    {
        field_constraints.reserve(fields_named.named.len());
        for field in &fields_named.named {
            match crate::parser::schema::schema_attrs::try_extract_schema_constraints(&field.attrs)
            {
                Ok(constraints) => field_constraints.push(constraints),
                Err(error) => return (None, error.to_compile_error()),
            }
        }
    }

    // Check for custom schema settings from #[schema(...)] attributes in one pass.
    let schema_attr = collect_schema_attribute_summary(&input.attrs);
    let schema_name = schema_attr.name.unwrap_or_else(|| name.to_string());

    // Extract default values from serde(default = "fn_name") attributes at derive time.
    // Span::call_site().local_file() returns None in unit tests — the map/unwrap_or_default
    // chain ensures the line is always executed even when the closure is not entered.
    let call_site_file = proc_macro2::Span::call_site().local_file();
    let field_defaults = call_site_file
        .as_deref()
        .map(|file_path| extract_field_defaults_from_path(input, file_path))
        .unwrap_or_default();

    if let Err(error) = validate_serde_default_values(input, &field_defaults) {
        return (None, error.to_compile_error());
    }

    // Schema-derived types appear in OpenAPI spec (include_in_openapi: true)
    let mut metadata = StructMetadata::new(schema_name, quote::quote!(#input).to_string());
    if let Some(source_identity) = derive_source_identity(input, call_site_file.as_deref()) {
        metadata = metadata.with_source_identity(source_identity);
    }
    if schema_attr.has_ref_override {
        metadata.include_in_openapi = false;
    }
    metadata.field_defaults = field_defaults;

    // When the `validation` feature is enabled on `vespera_macro`,
    // additionally emit `impl ::vespera::__validation::garde::Validate
    // for #StructName { ... }` so the field-level `#[schema(...)]`
    // constraints carry runtime checks alongside their OpenAPI metadata.
    // The emit function returns an empty `TokenStream` when no field
    // requests a runtime rule or when the feature is off.
    let garde = crate::garde_emit::emit_garde_validate(input, &field_constraints);
    // Emit the `::vespera::Schema` marker impl + per-field
    // `T: ::vespera::Schema` leaf assertions: a field of a custom type
    // that forgot its own `#[derive(Schema)]` becomes a compile error
    // instead of a silent `{type:"object"}` in the spec. Additive — it
    // does not change the emitted OpenAPI bytes for any field.
    let supplements = crate::schema_assertions::emit_schema_supplements(input, &field_constraints);
    let expanded = quote::quote! {
        #garde
        #supplements
    };
    (Some(metadata), expanded)
}

/// Extract default values from `#[serde(default = "fn_name")]` attributes
/// using the given source file path.
///
/// Separated from [`extract_field_defaults`] for testability: `Span::call_site().local_file()`
/// returns `None` in unit tests, so this function accepts the path directly.
pub fn extract_field_defaults_from_path(
    input: &syn::DeriveInput,
    file_path: &Path,
) -> BTreeMap<String, serde_json::Value> {
    let mut defaults = BTreeMap::new();

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(named) => &named.named,
            _ => return defaults,
        },
        _ => return defaults,
    };

    // Collect fields with function-based defaults
    let fn_defaults: Vec<(String, String)> = fields
        .iter()
        .filter_map(|f| {
            let field_name = f.ident.as_ref()?.to_string();
            if let Some(Some(fn_name)) = crate::parser::extract_default(&f.attrs) {
                // Only handle simple function names (not paths like "crate::utils::default")
                if fn_name.contains("::") {
                    None
                } else {
                    Some((field_name, fn_name))
                }
            } else {
                None
            }
        })
        .collect();

    if fn_defaults.is_empty() {
        return defaults;
    }

    defaults.extend(extract_defaults_from_path(&fn_defaults, file_path));
    defaults
}

fn validate_serde_default_values(
    input: &syn::DeriveInput,
    field_defaults: &BTreeMap<String, serde_json::Value>,
) -> Result<(), syn::Error> {
    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(named) => &named.named,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    let mut errors: Option<syn::Error> = None;
    for field in fields {
        let Some(default_kind) = extract_default(&field.attrs) else {
            continue;
        };
        if has_schema_default(&field.attrs)
            || serde_default_is_resolvable(field, default_kind.as_ref(), field_defaults)
        {
            continue;
        }

        let field_name = field.ident.as_ref().map_or_else(
            || "unknown".to_string(),
            |ident| strip_raw_prefix_owned(ident.to_string()),
        );
        let error = syn::Error::new_spanned(
            field,
            format!(
                "cannot statically determine the OpenAPI default for field `{field_name}` which has `#[serde(default)]`; add an explicit `#[schema(default = \"...\")]`"
            ),
        );
        if let Some(existing) = &mut errors {
            existing.combine(error);
        } else {
            errors = Some(error);
        }
    }

    errors.map_or(Ok(()), Err)
}

fn serde_default_is_resolvable(
    field: &syn::Field,
    default_kind: Option<&String>,
    field_defaults: &BTreeMap<String, serde_json::Value>,
) -> bool {
    match default_kind {
        Some(_) => field
            .ident
            .as_ref()
            .is_some_and(|ident| field_defaults.contains_key(&ident.to_string())),
        None => crate::schema_macro::type_utils::get_type_default(&field.ty).is_some(),
    }
}

fn has_schema_default(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("schema"))
        .any(|attr| {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    found = true;
                }
                Ok(())
            });
            found
        })
}

fn extract_defaults_from_path(
    fn_defaults: &[(String, String)],
    file_path: &Path,
) -> BTreeMap<String, serde_json::Value> {
    let Some(function_defaults) = cached_default_functions(file_path) else {
        return BTreeMap::new();
    };
    fn_defaults
        .iter()
        .filter_map(|(field_name, fn_name)| {
            function_defaults
                .get(fn_name)
                .cloned()
                .map(|value| (field_name.clone(), value))
        })
        .collect()
}

fn cached_default_functions(file_path: &Path) -> Option<Arc<BTreeMap<String, serde_json::Value>>> {
    // Fingerprint via the SHARED per-epoch file cache: this populates the
    // epoch cache so the `get_parsed_file` below reuses it instead of issuing
    // a second `fs::metadata` syscall (the previous direct `fs::metadata` here
    // double-stat'd every derive with function defaults). The mtime+len
    // fingerprint also matches the file-content cache, so a size-changing
    // timestamp-preserving edit invalidates this cache too.
    let fingerprint = get_file_fingerprint(file_path)?;
    if let Some(values) = DEFAULT_FUNCTION_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(file_path)
        .and_then(|entry| (entry.fingerprint == fingerprint).then(|| Arc::clone(&entry.values)))
    {
        return Some(values);
    }

    let file_ast = crate::schema_macro::file_cache::get_parsed_file(file_path)?;
    let values = Arc::new(extract_default_functions_from_file(&file_ast));
    DEFAULT_FUNCTION_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            file_path.to_path_buf(),
            DefaultFunctionCacheEntry {
                fingerprint,
                values: Arc::clone(&values),
            },
        );
    Some(values)
}

fn extract_default_functions_from_file(
    file_ast: &syn::File,
) -> BTreeMap<String, serde_json::Value> {
    file_ast
        .items
        .iter()
        .filter_map(|item| {
            let syn::Item::Fn(func) = item else {
                return None;
            };
            crate::openapi_generator::extract_default_value_from_function(func)
                .map(|value| (func.sig.ident.to_string(), value))
        })
        .collect()
}

/// Extract default values by finding functions in the given file AST.
/// Separated from `extract_field_defaults` for testability (proc_macro2::Span
/// is not available in unit tests).
#[cfg(test)]
pub fn extract_defaults_from_file(
    fn_defaults: &[(String, String)],
    file_ast: &syn::File,
) -> BTreeMap<String, serde_json::Value> {
    let mut defaults = BTreeMap::new();
    for (field_name, fn_name) in fn_defaults {
        if let Some(func) = crate::openapi_generator::find_function_in_file(file_ast, fn_name)
            && let Some(value) = crate::openapi_generator::extract_default_value_from_function(func)
        {
            defaults.insert(field_name.clone(), value);
        }
    }
    defaults
}

#[cfg(test)]
#[path = "schema_impl/tests.rs"]
mod tests;
