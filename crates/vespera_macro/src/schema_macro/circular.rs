//! Circular reference detection and handling
//!
//! Provides functions to detect and handle circular references between
//! `SeaORM` models when generating schema types.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;

use super::{
    seaorm::relation_target_module,
    type_utils::{
        SeaOrmRelationKind, capitalize_first, first_generic_type_arg, is_seaorm_relation_type,
        seaorm_relation_inner_type, seaorm_relation_kind,
    },
};
use crate::parser::extract_skip;

/// Combined result of circular reference analysis.
///
/// Produced by [`analyze_circular_refs()`] which parses a definition string once
/// and extracts both pieces of information in one field walk.
#[derive(Clone)]
pub struct CircularAnalysis {
    /// Field names that would create circular references.
    pub circular_fields: Vec<String>,
    /// Whether the model has any `BelongsTo` or `HasOne` relations (FK-based).
    pub has_fk_relations: bool,
}

/// Analyze the related model `definition` (defined in `related_module_path`)
/// of a relation of the model in `source_module_path`: which of its fields
/// lead back to the source, and whether it has FK relations at all.
///
/// A `HasOne` / `BelongsTo` field is circular when the entity it targets is
/// the source model, or reaches the source through further `HasOne` /
/// `BelongsTo` relations (`a -> b -> c -> a`): loading it through the
/// target's own `from_model` would recurse forever (and does not compile —
/// `E0733`). A self-referential relation of the source (`parent:
/// BelongsTo<Entity>`) is circular too. Without a related module the
/// historical name heuristics apply (`<source>::Schema` / `<Source>Schema`).
pub fn analyze_circular_refs(
    source_module_path: &[String],
    related_module_path: &[String],
    definition: &str,
) -> CircularAnalysis {
    let mut analysis = CircularAnalysis {
        circular_fields: Vec::new(),
        has_fk_relations: false,
    };
    let Ok(parsed) = super::file_cache::parse_struct_cached(definition) else {
        return analysis;
    };
    let syn::Fields::Named(fields_named) = &parsed.fields else {
        return analysis;
    };

    let source_module = source_module_path
        .last()
        .map_or("", std::string::String::as_str);
    let capitalized_pattern = format!("{}Schema", capitalize_first(source_module));
    let absolute = |path: &[String]| path.first().is_some_and(|first| first == "crate");
    let module_aware = absolute(source_module_path) && absolute(related_module_path);

    for field in &fields_named.named {
        // FieldsNamed guarantees all fields have identifiers
        let field_name = field
            .ident
            .as_ref()
            .expect("named field has ident")
            .to_string();
        let kind = seaorm_relation_kind(&field.ty);
        if let Some(kind) = kind {
            analysis.has_fk_relations |= kind.is_fk_backed();
        }

        let circular = match kind {
            // HasMany is excluded by default and is never loaded recursively.
            Some(SeaOrmRelationKind::HasMany) => false,
            Some(_) if module_aware => relation_target_module(&field.ty, related_module_path)
                .is_some_and(|target| {
                    target == source_module_path
                        || reaches_module(
                            &target,
                            source_module_path,
                            &mut HashSet::from([related_module_path.to_vec()]),
                        )
                }),
            Some(_) => seaorm_relation_inner_type(&field.ty).is_some_and(|inner| {
                type_targets_source_schema(inner, source_module, &capitalized_pattern)
            }),
            None => type_targets_source_schema(&field.ty, source_module, &capitalized_pattern),
        };
        if circular {
            analysis.circular_fields.push(field_name);
        }
    }
    analysis
}

/// Whether the `Model` of `module` reaches `target` through `HasOne` /
/// `BelongsTo` relations (the ones `from_model` loads recursively).
fn reaches_module(
    module: &[String],
    target: &[String],
    visited: &mut HashSet<Vec<String>>,
) -> bool {
    if !visited.insert(module.to_vec()) {
        return false;
    }
    // A stored definition always parses.
    let Some(parsed) =
        super::file_cache::get_struct_from_schema_path(&format!("{}::Model", module.join("::")))
            .and_then(|model| super::file_cache::parse_struct_cached(&model.definition).ok())
    else {
        return false;
    };
    parsed
        .fields
        .iter()
        .filter(|field| {
            seaorm_relation_kind(&field.ty).is_some_and(SeaOrmRelationKind::is_fk_backed)
        })
        .filter_map(|field| relation_target_module(&field.ty, module))
        .any(|next| next == target || reaches_module(&next, target, visited))
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

/// Generate inline type construction for `from_model`.
///
/// When we have an inline type (e.g., `MemoResponseRel_User`), this function generates
/// the construction code that only includes the fields present in the inline type
/// (each assignment keeps the field's `#[cfg(..)]`, like the inline type).
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
            // (the others were excluded: circular reference or otherwise).
            included_fields.contains(&field_name).then(|| {
                // The inline type hoists a `cfg_attr(p, cfg(q))` too.
                let cfgs = super::field_attrs::field_cfgs(&field.attrs);
                quote! { #(#cfgs)* #field_ident: #var_ident.#field_ident }
            })
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
