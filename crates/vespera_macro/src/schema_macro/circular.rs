//! Circular reference detection and handling
//!
//! Provides functions to detect and handle circular references between
//! `SeaORM` models when generating schema types.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;

use super::{
    seaorm::extract_belongs_to_from_field,
    type_utils::{
        SeaOrmRelationKind, capitalize_first, first_generic_type_arg, is_option_type,
        is_seaorm_relation_type, seaorm_relation_inner_type, seaorm_relation_kind,
    },
};
use crate::parser::extract_skip;

/// Combined result of circular reference analysis.
///
/// Produced by [`analyze_circular_refs()`] which parses a definition string once
/// and extracts all three pieces of information that would otherwise require
/// three separate parse calls.
#[derive(Clone)]
pub struct CircularAnalysis {
    /// Field names that would create circular references.
    pub circular_fields: Vec<String>,
    /// Whether the model has any `BelongsTo` or `HasOne` relations (FK-based).
    pub has_fk_relations: bool,
    /// For each `HasOne`/`BelongsTo` field, whether the FK is required (not `Option`).
    ///
    /// Keyed by field name. Contains entries for ALL `HasOne`/`BelongsTo` fields,
    /// not just circular ones, so callers can look up any relation field.
    pub circular_field_required: HashMap<String, bool>,
}

/// Analyze a struct definition for circular references, FK relations, and FK optionality
/// in a single parse + single field walk.
///
/// Parses the definition string once and extracts all circular reference
/// information in a single field walk.
pub fn analyze_circular_refs(source_module_path: &[String], definition: &str) -> CircularAnalysis {
    let Ok(parsed) = super::file_cache::parse_struct_cached(definition) else {
        return CircularAnalysis {
            circular_fields: Vec::new(),
            has_fk_relations: false,
            circular_field_required: HashMap::new(),
        };
    };

    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return CircularAnalysis {
            circular_fields: Vec::new(),
            has_fk_relations: false,
            circular_field_required: HashMap::new(),
        };
    };

    let source_module = source_module_path
        .last()
        .map_or("", std::string::String::as_str);

    let mut circular_fields = Vec::with_capacity(fields_named.named.len());
    let mut has_fk = false;
    let mut circular_field_required = HashMap::with_capacity(fields_named.named.len());

    // Pre-build field name → &Field index for O(1) FK column lookup
    // instead of O(N) linear search per FK relation
    let field_by_name: HashMap<String, &syn::Field> = fields_named
        .named
        .iter()
        .filter_map(|f| f.ident.as_ref().map(|id| (id.to_string(), f)))
        .collect();
    let capitalized_pattern = format!("{}Schema", capitalize_first(source_module));

    for field in &fields_named.named {
        // FieldsNamed guarantees all fields have identifiers
        let field_ident = field.ident.as_ref().expect("named field has ident");
        let field_name = field_ident.to_string();
        // --- has_fk_relations logic ---
        if seaorm_relation_kind(&field.ty).is_some_and(SeaOrmRelationKind::is_fk_backed) {
            has_fk = true;

            // --- is_circular_relation_required logic (for ALL FK fields) ---
            let required = extract_belongs_to_from_field(&field.attrs).is_some_and(|fk| {
                field_by_name
                    .get(&fk)
                    .is_some_and(|f| !is_option_type(&f.ty))
            });
            circular_field_required.insert(field_name.clone(), required);
        }

        // --- detect_circular_fields logic ---
        // Skip HasMany — they are excluded by default and don't create circular refs.
        if is_circular_relation_type(&field.ty, source_module, &capitalized_pattern) {
            circular_fields.push(field_name);
        }
    }

    CircularAnalysis {
        circular_fields,
        has_fk_relations: has_fk,
        circular_field_required,
    }
}

fn is_circular_relation_type(
    ty: &syn::Type,
    source_module: &str,
    capitalized_schema: &str,
) -> bool {
    match seaorm_relation_kind(ty) {
        Some(SeaOrmRelationKind::HasMany) => false,
        Some(SeaOrmRelationKind::HasOne | SeaOrmRelationKind::BelongsTo) => {
            seaorm_relation_inner_type(ty).is_some_and(|inner| {
                type_targets_source_schema(inner, source_module, capitalized_schema)
            })
        }
        None => type_targets_source_schema(ty, source_module, capitalized_schema),
    }
}

fn transparent_inner_type<'a>(ty: &'a syn::Type, wrapper: &str) -> Option<&'a syn::Type> {
    let syn::Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != wrapper {
        return None;
    }
    first_generic_type_arg(segment)
}

fn type_targets_source_schema(
    ty: &syn::Type,
    source_module: &str,
    capitalized_schema: &str,
) -> bool {
    if let Some(inner) =
        transparent_inner_type(ty, "Option").or_else(|| transparent_inner_type(ty, "Box"))
    {
        return type_targets_source_schema(inner, source_module, capitalized_schema);
    }
    let syn::Type::Path(type_path) = ty else {
        return false;
    };
    let segments: Vec<_> = type_path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    match segments.as_slice() {
        [last] => last == capitalized_schema,
        [.., module, last] => {
            module == source_module && (last == "Schema" || last == "Entity")
                || last == capitalized_schema
        }
        [] => false,
    }
}

/// Generate a default value for a `SeaORM` relation field in inline construction.
///
/// - `HasMany<T>` -> `vec![]`
/// - `HasOne<T>`/`BelongsTo<T>` with optional FK -> `None`
/// - `HasOne<T>`/`BelongsTo<T>` with required FK -> needs parent stub (handled separately)
pub fn generate_default_for_relation_field(
    ty: &syn::Type,
    field_ident: &syn::Ident,
    field_attrs: &[syn::Attribute],
    all_fields: &syn::FieldsNamed,
) -> TokenStream {
    // Check the SeaORM relation type using the parsed AST rather than rendered tokens.
    if seaorm_relation_kind(ty) == Some(SeaOrmRelationKind::HasMany) {
        // HasMany -> Vec<Schema> -> empty vec
        quote! { #field_ident: vec![] }
    } else if seaorm_relation_kind(ty).is_some_and(SeaOrmRelationKind::is_fk_backed) {
        // Check FK field optionality
        let fk_field = extract_belongs_to_from_field(field_attrs);
        let is_optional = fk_field.as_ref().is_none_or(|fk| {
            all_fields.named.iter().any(|f| {
                f.ident.as_ref().map(std::string::ToString::to_string) == Some(fk.clone())
                    && is_option_type(&f.ty)
            })
        });

        if is_optional {
            // Option<Box<Schema>> -> None
            quote! { #field_ident: None }
        } else {
            // Box<Schema> (required) -> use __parent_stub__
            // This variable will be defined by the caller when needed
            quote! { #field_ident: Box::new(__parent_stub__.clone()) }
        }
    } else {
        // Unknown relation type - try Default::default()
        quote! { #field_ident: Default::default() }
    }
}

/// Generate inline struct construction for a related schema, excluding circular fields.
///
/// Instead of `<user::Schema as From<_>>::from(r)`, generates:
/// ```ignore
/// user::Schema {
///     id: r.id,
///     name: r.name,
///     memos: vec![], // circular field - use default
/// }
/// ```
pub fn generate_inline_struct_construction(
    schema_path: &TokenStream,
    related_schema_def: &str,
    circular_fields: &[String],
    var_name: &str,
) -> TokenStream {
    // Parse the related schema definition
    let Ok(parsed) = super::file_cache::parse_struct_cached(related_schema_def) else {
        // Fallback to From::from if parsing fails
        let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());
        return quote! { <#schema_path as From<_>>::from(#var_ident) };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields for FK checking
    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return quote! { <#schema_path as From<_>>::from(#var_ident) };
    };

    let field_assignments: Vec<TokenStream> = fields_named
        .named
        .iter()
        .filter_map(|field| {
            let field_ident = field.ident.as_ref()?;
            let field_name = field_ident.to_string();

            // Skip fields marked with serde(skip)
            if extract_skip(&field.attrs) {
                return None;
            }

            if circular_fields.contains(&field_name) || is_seaorm_relation_type(&field.ty) {
                // Circular field or relation field - generate appropriate default
                // based on the SeaORM relation type
                Some(generate_default_for_relation_field(
                    &field.ty,
                    field_ident,
                    &field.attrs,
                    fields_named,
                ))
            } else {
                // Regular field - copy from model
                Some(quote! { #field_ident: #var_ident.#field_ident })
            }
        })
        .collect();

    quote! {
        #schema_path {
            #(#field_assignments),*
        }
    }
}

/// Generate inline type construction for `from_model`.
///
/// When we have an inline type (e.g., `MemoResponseRel_User`), this function generates
/// the construction code that only includes the fields present in the inline type.
///
/// ```ignore
/// MemoResponseRel_User {
///     id: r.id,
///     name: r.name,
///     email: r.email,
///     // memos field is NOT included - it was excluded from inline type
/// }
/// ```
pub fn generate_inline_type_construction(
    inline_type_name: &syn::Ident,
    included_fields: &[String],
    related_model_def: &str,
    var_name: &str,
) -> TokenStream {
    // Parse the related model definition
    let Ok(parsed) = super::file_cache::parse_struct_cached(related_model_def) else {
        // Fallback to Default if parsing fails
        return quote! { Default::default() };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields
    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return quote! { Default::default() };
    };

    let field_assignments: Vec<TokenStream> = fields_named
        .named
        .iter()
        .filter_map(|field| {
            let field_ident = field.ident.as_ref()?;
            let field_name = field_ident.to_string();

            // Skip fields marked with serde(skip)
            if extract_skip(&field.attrs) {
                return None;
            }

            // Skip relation fields (they are not in the inline type)
            if is_seaorm_relation_type(&field.ty) {
                return None;
            }

            // Only include fields that are in the inline type's field list
            if included_fields.contains(&field_name) {
                // Regular field - copy from model
                Some(quote! { #field_ident: #var_ident.#field_ident })
            } else {
                // This field was excluded (circular reference or otherwise)
                None
            }
        })
        .collect();

    quote! {
        #inline_type_name {
            #(#field_assignments),*
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn plain_and_empty_path_types_do_not_require_relation_wrappers() {
        let plain: syn::Type = syn::parse_str(std::hint::black_box("MemoSchema")).unwrap();
        assert!(is_circular_relation_type(
            &plain,
            std::hint::black_box("memo"),
            std::hint::black_box("MemoSchema")
        ));

        let empty = syn::Type::Path(syn::TypePath {
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        assert!(!is_circular_relation_type(
            std::hint::black_box(&empty),
            "memo",
            "MemoSchema"
        ));
    }
}
