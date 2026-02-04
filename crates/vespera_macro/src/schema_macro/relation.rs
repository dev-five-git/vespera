//! SeaORM relation type handling
//!
//! Contains functions for:
//! - Converting SeaORM relation types (HasOne, HasMany, BelongsTo) to schema types
//! - Detecting circular references between schemas
//! - Extracting relation metadata from attributes

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::type_utils::{capitalize_first, is_option_type};
use crate::parser::extract_skip;

/// Relation field info for generating from_model code
#[derive(Clone)]
pub struct RelationFieldInfo {
    /// Field name in the generated struct
    pub field_name: syn::Ident,
    /// Relation type: "HasOne", "HasMany", or "BelongsTo"
    pub relation_type: String,
    /// Target Schema path (e.g., crate::models::user::Schema)
    pub schema_path: TokenStream,
    /// Whether the relation is optional
    pub is_optional: bool,
    /// If Some, this relation has circular refs and uses an inline type
    /// Contains: (inline_type_name, circular_fields_to_exclude)
    pub inline_type_info: Option<(syn::Ident, Vec<String>)>,
}

/// Extract the "from" field name from a sea_orm belongs_to attribute.
/// e.g., `#[sea_orm(belongs_to, from = "user_id", to = "id")]` → Some("user_id")
pub fn extract_belongs_to_from_field(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("sea_orm") {
            let mut from_field = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("from")
                    && let Ok(value) = meta.value()
                    && let Ok(lit) = value.parse::<syn::LitStr>()
                {
                    from_field = Some(lit.value());
                }
                Ok(())
            });
            if from_field.is_some() {
                return from_field;
            }
        }
    }
    None
}

/// Check if a field in the struct is optional (Option<T>).
pub fn is_field_optional_in_struct(struct_item: &syn::ItemStruct, field_name: &str) -> bool {
    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            if let Some(ident) = &field.ident
                && ident == field_name
            {
                return is_option_type(&field.ty);
            }
        }
    }
    false
}

/// Convert a SeaORM relation type to a Schema type AND return relation info.
///
/// - `#[sea_orm(has_one)]` → Always `Option<Box<Schema>>`
/// - `#[sea_orm(has_many)]` → Always `Vec<Schema>`
/// - `#[sea_orm(belongs_to, from = "field")]`:
///   - If `from` field is `Option<T>` → `Option<Box<Schema>>`
///   - If `from` field is required → `Box<Schema>`
///
/// The `source_module_path` is used to resolve relative paths like `super::`.
/// e.g., if source is `crate::models::memo::Model`, module path is `crate::models::memo`
///
/// Returns None if the type is not a relation type or conversion fails.
/// Returns (TokenStream, RelationFieldInfo) on success for use in from_model generation.
pub fn convert_relation_type_to_schema_with_info(
    ty: &Type,
    field_attrs: &[syn::Attribute],
    parsed_struct: &syn::ItemStruct,
    source_module_path: &[String],
    field_name: syn::Ident,
) -> Option<(TokenStream, RelationFieldInfo)> {
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return None,
    };

    let segment = type_path.path.segments.last()?;
    let ident_str = segment.ident.to_string();

    // Check if this is a relation type with generic argument
    let args = match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => args,
        _ => return None,
    };

    // Get the inner Entity type
    let inner_ty = match args.args.first()? {
        syn::GenericArgument::Type(ty) => ty,
        _ => return None,
    };

    // Extract the path and convert to absolute Schema path
    let inner_path = match inner_ty {
        Type::Path(tp) => tp,
        _ => return None,
    };

    // Collect segments as strings
    let segments: Vec<String> = inner_path
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();

    // Convert path to absolute, resolving `super::` relative to source module
    let absolute_segments: Vec<String> = if !segments.is_empty() && segments[0] == "super" {
        let super_count = segments.iter().take_while(|s| *s == "super").count();
        let parent_path_len = source_module_path.len().saturating_sub(super_count);
        let mut abs = source_module_path[..parent_path_len].to_vec();
        for seg in segments.iter().skip(super_count) {
            if seg == "Entity" {
                abs.push("Schema".to_string());
            } else {
                abs.push(seg.clone());
            }
        }
        abs
    } else if !segments.is_empty() && segments[0] == "crate" {
        segments
            .iter()
            .map(|s| {
                if s == "Entity" {
                    "Schema".to_string()
                } else {
                    s.clone()
                }
            })
            .collect()
    } else {
        let parent_path_len = source_module_path.len().saturating_sub(1);
        let mut abs = source_module_path[..parent_path_len].to_vec();
        for seg in &segments {
            if seg == "Entity" {
                abs.push("Schema".to_string());
            } else {
                abs.push(seg.clone());
            }
        }
        abs
    };

    // Build the absolute path as tokens
    let path_idents: Vec<syn::Ident> = absolute_segments
        .iter()
        .map(|s| syn::Ident::new(s, proc_macro2::Span::call_site()))
        .collect();
    let schema_path = quote! { #(#path_idents)::* };

    // Convert based on relation type
    match ident_str.as_str() {
        "HasOne" => {
            // HasOne → Check FK field to determine optionality
            // If FK is Option<T> → relation is optional: Option<Box<Schema>>
            // If FK is required → relation is required: Box<Schema>
            let fk_field = extract_belongs_to_from_field(field_attrs);
            let is_optional = fk_field
                .as_ref()
                .map(|f| is_field_optional_in_struct(parsed_struct, f))
                .unwrap_or(true); // Default to optional if we can't determine

            let converted = if is_optional {
                quote! { Option<Box<#schema_path>> }
            } else {
                quote! { Box<#schema_path> }
            };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "HasOne".to_string(),
                schema_path: schema_path.clone(),
                is_optional,
                inline_type_info: None, // Will be populated later if circular
            };
            Some((converted, info))
        }
        "HasMany" => {
            let converted = quote! { Vec<#schema_path> };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "HasMany".to_string(),
                schema_path: schema_path.clone(),
                is_optional: false,
                inline_type_info: None, // Will be populated later if circular
            };
            Some((converted, info))
        }
        "BelongsTo" => {
            // BelongsTo → Check FK field to determine optionality
            // If FK is Option<T> → relation is optional: Option<Box<Schema>>
            // If FK is required → relation is required: Box<Schema>
            let fk_field = extract_belongs_to_from_field(field_attrs);
            let is_optional = fk_field
                .as_ref()
                .map(|f| is_field_optional_in_struct(parsed_struct, f))
                .unwrap_or(true); // Default to optional if we can't determine

            let converted = if is_optional {
                quote! { Option<Box<#schema_path>> }
            } else {
                quote! { Box<#schema_path> }
            };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "BelongsTo".to_string(),
                schema_path: schema_path.clone(),
                is_optional,
                inline_type_info: None, // Will be populated later if circular
            };
            Some((converted, info))
        }
        _ => None,
    }
}

/// Detect circular reference fields in a related schema.
///
/// When generating `MemoSchema.user`, we need to check if `UserSchema` has any fields
/// that reference back to `MemoSchema` via BelongsTo/HasOne (FK-based relations).
///
/// HasMany relations are NOT considered circular because they are excluded by default
/// from generated schemas.
///
/// Returns a list of field names that would create circular references.
pub fn detect_circular_fields(
    _source_schema_name: &str,
    source_module_path: &[String],
    related_schema_def: &str,
) -> Vec<String> {
    let mut circular_fields = Vec::new();

    // Parse the related schema definition
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_schema_def) else {
        return circular_fields;
    };

    // Get the source module name (e.g., "memo" from ["crate", "models", "memo"])
    let source_module = source_module_path.last().map(|s| s.as_str()).unwrap_or("");

    if let syn::Fields::Named(fields_named) = &parsed.fields {
        for field in &fields_named.named {
            let Some(field_ident) = &field.ident else {
                continue;
            };
            let field_name = field_ident.to_string();

            // Check if this field's type references the source schema
            let field_ty = &field.ty;
            let ty_str = quote!(#field_ty).to_string();

            // Normalize whitespace: quote!() produces "foo :: bar" instead of "foo::bar"
            // Remove all whitespace to make pattern matching reliable
            let ty_str_normalized = ty_str.replace(' ', "");

            // SKIP HasMany relations - they are excluded by default from schemas,
            // so they don't create actual circular references in the output
            if ty_str_normalized.contains("HasMany<") {
                continue;
            }

            // Check for BelongsTo/HasOne patterns that reference the source:
            // - HasOne<memo::Entity>
            // - BelongsTo<memo::Entity>
            // - Box<memo::Schema> (already converted)
            // - Option<Box<memo::Schema>>
            let is_circular = (ty_str_normalized.contains("HasOne<")
                || ty_str_normalized.contains("BelongsTo<")
                || ty_str_normalized.contains("Box<"))
                && (ty_str_normalized.contains(&format!("{}::Schema", source_module))
                    || ty_str_normalized.contains(&format!("{}::Entity", source_module))
                    || ty_str_normalized
                        .contains(&format!("{}Schema", capitalize_first(source_module))));

            if is_circular {
                circular_fields.push(field_name);
            }
        }
    }

    circular_fields
}

/// Check if a Model has any BelongsTo or HasOne relations (FK-based relations).
///
/// This is used to determine if the target schema has `from_model()` method
/// (async, with DB) or simple `From<Model>` impl (sync, no DB).
///
/// - Schemas with FK relations → have `from_model()`, need async call
/// - Schemas without FK relations → have `From<Model>`, can use sync conversion
pub fn has_fk_relations(model_def: &str) -> bool {
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(model_def) else {
        return false;
    };

    if let syn::Fields::Named(fields_named) = &parsed.fields {
        for field in &fields_named.named {
            let field_ty = &field.ty;
            let ty_str = quote!(#field_ty).to_string().replace(' ', "");

            // Check for BelongsTo or HasOne patterns
            if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
                return true;
            }
        }
    }

    false
}

/// Check if a circular relation field in the related schema is required (Box<T>) or optional (Option<Box<T>>).
///
/// Returns true if the circular relation is required and needs a parent stub.
pub fn is_circular_relation_required(related_model_def: &str, circular_field_name: &str) -> bool {
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_model_def) else {
        return false;
    };

    if let syn::Fields::Named(fields_named) = &parsed.fields {
        for field in &fields_named.named {
            let Some(field_ident) = &field.ident else {
                continue;
            };
            if *field_ident != circular_field_name {
                continue;
            }

            // Check if this is a HasOne/BelongsTo with required FK
            let ty_str = quote!(#field.ty).to_string().replace(' ', "");
            if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
                // Check FK field optionality
                let fk_field = extract_belongs_to_from_field(&field.attrs);
                if let Some(fk) = fk_field {
                    // Find FK field and check if it's Option
                    for f in &fields_named.named {
                        if f.ident.as_ref().map(|i| i.to_string()) == Some(fk.clone()) {
                            return !is_option_type(&f.ty);
                        }
                    }
                }
            }
        }
    }
    false
}

/// Generate a default value for a SeaORM relation field in inline construction.
///
/// - `HasMany<T>` → `vec![]`
/// - `HasOne<T>`/`BelongsTo<T>` with optional FK → `None`
/// - `HasOne<T>`/`BelongsTo<T>` with required FK → needs parent stub (handled separately)
pub fn generate_default_for_relation_field(
    ty: &Type,
    field_ident: &syn::Ident,
    field_attrs: &[syn::Attribute],
    all_fields: &syn::FieldsNamed,
) -> TokenStream {
    let ty_str = quote!(#ty).to_string().replace(' ', "");

    // Check the SeaORM relation type
    if ty_str.contains("HasMany<") {
        // HasMany → Vec<Schema> → empty vec
        quote! { #field_ident: vec![] }
    } else if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
        // Check FK field optionality
        let fk_field = extract_belongs_to_from_field(field_attrs);
        let is_optional = fk_field
            .as_ref()
            .map(|fk| {
                all_fields.named.iter().any(|f| {
                    f.ident.as_ref().map(|i| i.to_string()) == Some(fk.clone())
                        && is_option_type(&f.ty)
                })
            })
            .unwrap_or(true);

        if is_optional {
            // Option<Box<Schema>> → None
            quote! { #field_ident: None }
        } else {
            // Box<Schema> (required) → use __parent_stub__
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
    use super::type_utils::is_seaorm_relation_type;

    // Parse the related schema definition
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_schema_def) else {
        // Fallback to From::from if parsing fails
        let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());
        return quote! { <#schema_path as From<_>>::from(#var_ident) };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields for FK checking
    let fields_named = match &parsed.fields {
        syn::Fields::Named(f) => f,
        _ => {
            return quote! { <#schema_path as From<_>>::from(#var_ident) };
        }
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

/// Generate inline type construction for from_model.
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
    use super::type_utils::is_seaorm_relation_type;

    // Parse the related model definition
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(related_model_def) else {
        // Fallback to Default if parsing fails
        return quote! { Default::default() };
    };

    let var_ident = syn::Ident::new(var_name, proc_macro2::Span::call_site());

    // Get the named fields
    let fields_named = match &parsed.fields {
        syn::Fields::Named(f) => f,
        _ => {
            return quote! { Default::default() };
        }
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

/// Build Entity path from Schema path.
/// e.g., `crate::models::user::Schema` -> `crate::models::user::Entity`
pub fn build_entity_path_from_schema_path(
    schema_path: &TokenStream,
    _source_module_path: &[String],
) -> TokenStream {
    // Parse the schema path to extract segments
    let path_str = schema_path.to_string();
    let segments: Vec<&str> = path_str.split("::").map(|s| s.trim()).collect();

    // Replace "Schema" with "Entity" in the last segment
    let entity_segments: Vec<String> = segments
        .iter()
        .map(|s| {
            if *s == "Schema" {
                "Entity".to_string()
            } else {
                s.to_string()
            }
        })
        .collect();

    // Build the path tokens
    let path_idents: Vec<syn::Ident> = entity_segments
        .iter()
        .map(|s| syn::Ident::new(s, proc_macro2::Span::call_site()))
        .collect();

    quote! { #(#path_idents)::* }
}
