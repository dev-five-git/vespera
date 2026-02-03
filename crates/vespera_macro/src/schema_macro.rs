//! Schema macro implementation
//!
//! Provides macros for generating OpenAPI schemas from struct types:
//! - `schema!` - Generate Schema value with optional field filtering
//! - `schema_type!` - Generate new struct type derived from existing type

use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use std::path::Path;
use syn::punctuated::Punctuated;
use syn::{Ident, LitStr, Token, Type, bracketed, parenthesized, parse::Parse, parse::ParseStream};

use crate::metadata::StructMetadata;
use crate::parser::{
    extract_default, extract_field_rename, extract_rename_all, extract_skip,
    extract_skip_serializing_if, parse_type_to_schema_ref, rename_field, strip_raw_prefix,
};
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

/// Input for the schema! macro
///
/// Supports:
/// - `schema!(Type)` - Full schema
/// - `schema!(Type, omit = ["field1", "field2"])` - Schema with fields omitted
/// - `schema!(Type, pick = ["field1", "field2"])` - Schema with only specified fields (future)
pub struct SchemaInput {
    /// The type to generate schema for
    pub ty: Type,
    /// Fields to omit from the schema
    pub omit: Option<Vec<String>>,
    /// Fields to pick (include only these fields)
    pub pick: Option<Vec<String>>,
}

impl Parse for SchemaInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse the type
        let ty: Type = input.parse()?;

        let mut omit = None;
        let mut pick = None;

        // Parse optional parameters
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "omit" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    omit = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                "pick" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    pick = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown parameter: `{}`. Expected `omit` or `pick`",
                            ident_str
                        ),
                    ));
                }
            }
        }

        // Validate: can't use both omit and pick
        if omit.is_some() && pick.is_some() {
            return Err(syn::Error::new(
                input.span(),
                "cannot use both `omit` and `pick` in the same schema! invocation",
            ));
        }

        Ok(SchemaInput { ty, omit, pick })
    }
}

/// Generate schema code from a struct with optional field filtering
pub fn generate_schema_code(
    input: &SchemaInput,
    schema_storage: &[StructMetadata],
) -> Result<TokenStream, syn::Error> {
    // Extract type name from the Type
    let type_name = extract_type_name(&input.ty)?;

    // Find struct definition in storage
    let struct_def = schema_storage
        .iter()
        .find(|s| s.name == type_name)
        .ok_or_else(|| {
            syn::Error::new_spanned(
                &input.ty,
                format!(
                    "type `{}` not found. Make sure it has #[derive(Schema)] before this macro invocation",
                    type_name
                ),
            )
        })?;

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = syn::parse_str(&struct_def.definition).map_err(|e| {
        syn::Error::new_spanned(
            &input.ty,
            format!(
                "failed to parse struct definition for `{}`: {}",
                type_name, e
            ),
        )
    })?;

    // Build omit set
    let omit_set: HashSet<String> = input.omit.clone().unwrap_or_default().into_iter().collect();

    // Build pick set
    let pick_set: HashSet<String> = input.pick.clone().unwrap_or_default().into_iter().collect();

    // Generate schema with filtering
    let schema_tokens =
        generate_filtered_schema(&parsed_struct, &omit_set, &pick_set, schema_storage)?;

    Ok(schema_tokens)
}

/// Extract type name from a Type
fn extract_type_name(ty: &Type) -> Result<String, syn::Error> {
    match ty {
        Type::Path(type_path) => {
            // Get the last segment (handles paths like crate::User)
            let segment = type_path.path.segments.last().ok_or_else(|| {
                syn::Error::new_spanned(ty, "expected a type path with at least one segment")
            })?;
            Ok(segment.ident.to_string())
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "expected a type path (e.g., `User` or `crate::User`)",
        )),
    }
}

/// Check if a type is a qualified path (has multiple segments like crate::models::User)
fn is_qualified_path(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path.path.segments.len() > 1,
        _ => false,
    }
}

/// Check if a type is a SeaORM relation type (HasOne, HasMany, BelongsTo)
fn is_seaorm_relation_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last() {
                let ident = segment.ident.to_string();
                matches!(ident.as_str(), "HasOne" | "HasMany" | "BelongsTo")
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check if a struct is a SeaORM Model (has #[sea_orm::model] or #[sea_orm(table_name = ...)] attribute)
fn is_seaorm_model(struct_item: &syn::ItemStruct) -> bool {
    for attr in &struct_item.attrs {
        // Check for #[sea_orm::model] or #[sea_orm(...)]
        let path = attr.path();
        if path.is_ident("sea_orm") {
            return true;
        }
        // Check for path like sea_orm::model
        let segments: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if segments.first().is_some_and(|s| s == "sea_orm") {
            return true;
        }
    }
    false
}

/// Check if a type name is a primitive or well-known type that doesn't need path resolution.
fn is_primitive_or_known_type(name: &str) -> bool {
    matches!(
        name,
        // Rust primitives
        "bool"
            | "char"
            | "str"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            // Common std types
            | "String"
            | "Vec"
            | "Option"
            | "Result"
            | "Box"
            | "Rc"
            | "Arc"
            | "HashMap"
            | "HashSet"
            | "BTreeMap"
            | "BTreeSet"
            // Chrono types
            | "DateTime"
            | "NaiveDateTime"
            | "NaiveDate"
            | "NaiveTime"
            | "Utc"
            | "Local"
            | "FixedOffset"
            // SeaORM types (will be converted separately)
            | "DateTimeWithTimeZone"
            | "DateTimeUtc"
            | "DateTimeLocal"
            // UUID
            | "Uuid"
            // Serde JSON
            | "Value"
    )
}

/// Resolve a simple type to an absolute path using the source module path.
///
/// For example, if source_module_path is ["crate", "models", "memo"] and
/// the type is `MemoStatus`, it returns `crate::models::memo::MemoStatus`.
///
/// If the type is already qualified (has `::`) or is a primitive/known type,
/// returns the original type unchanged.
fn resolve_type_to_absolute_path(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return quote! { #ty },
    };

    // If path has multiple segments (already qualified like `crate::foo::Bar`), return as-is
    if type_path.path.segments.len() > 1 {
        return quote! { #ty };
    }

    // Get the single segment
    let segment = match type_path.path.segments.first() {
        Some(s) => s,
        None => return quote! { #ty },
    };

    let ident_str = segment.ident.to_string();

    // If it's a primitive or known type, return as-is
    if is_primitive_or_known_type(&ident_str) {
        return quote! { #ty };
    }

    // If no source module path, return as-is
    if source_module_path.is_empty() {
        return quote! { #ty };
    }

    // Build absolute path: source_module_path + type_name
    let path_idents: Vec<syn::Ident> = source_module_path
        .iter()
        .map(|s| syn::Ident::new(s, proc_macro2::Span::call_site()))
        .collect();
    let type_ident = &segment.ident;
    let args = &segment.arguments;

    quote! { #(#path_idents)::* :: #type_ident #args }
}

/// Convert SeaORM datetime types to chrono equivalents.
///
/// This allows generated schemas to use standard chrono types instead of
/// requiring `use sea_orm::entity::prelude::DateTimeWithTimeZone`.
///
/// Conversions:
/// - `DateTimeWithTimeZone` → `chrono::DateTime<chrono::FixedOffset>`
/// - `DateTimeUtc` → `chrono::DateTime<chrono::Utc>`
/// - `DateTimeLocal` → `chrono::DateTime<chrono::Local>`
/// - `DateTime` (SeaORM) → `chrono::NaiveDateTime`
/// - `Date` (SeaORM) → `chrono::NaiveDate`
/// - `Time` (SeaORM) → `chrono::NaiveTime`
///
/// Returns the original type as TokenStream if not a SeaORM datetime type.
fn convert_seaorm_type_to_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return quote! { #ty },
    };

    let segment = match type_path.path.segments.last() {
        Some(s) => s,
        None => return quote! { #ty },
    };

    let ident_str = segment.ident.to_string();

    match ident_str.as_str() {
        // Use vespera::chrono to avoid requiring users to add chrono dependency
        "DateTimeWithTimeZone" => {
            quote! { vespera::chrono::DateTime<vespera::chrono::FixedOffset> }
        }
        "DateTimeUtc" => quote! { vespera::chrono::DateTime<vespera::chrono::Utc> },
        "DateTimeLocal" => quote! { vespera::chrono::DateTime<vespera::chrono::Local> },
        // Not a SeaORM datetime type - resolve to absolute path if needed
        _ => resolve_type_to_absolute_path(ty, source_module_path),
    }
}

/// Convert a type to chrono equivalent, handling Option<T> wrapper.
///
/// If the type is `Option<SeaOrmType>`, converts to `Option<ChronoType>`.
/// If the type is just `SeaOrmType`, converts to `ChronoType`.
///
/// Also resolves local types (like `MemoStatus`) to absolute paths
/// (like `crate::models::memo::MemoStatus`) using source_module_path.
fn convert_type_with_chrono(ty: &Type, source_module_path: &[String]) -> TokenStream {
    // Check if it's Option<T>
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.first()
        && segment.ident == "Option"
    {
        // Extract the inner type from Option<T>
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
        {
            let converted_inner = convert_seaorm_type_to_chrono(inner_ty, source_module_path);
            return quote! { Option<#converted_inner> };
        }
    }

    // Check if it's Vec<T>
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.first()
        && segment.ident == "Vec"
    {
        // Extract the inner type from Vec<T>
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
        {
            let converted_inner = convert_seaorm_type_to_chrono(inner_ty, source_module_path);
            return quote! { Vec<#converted_inner> };
        }
    }

    // Not Option or Vec, convert directly
    convert_seaorm_type_to_chrono(ty, source_module_path)
}

/// Relation field info for generating from_model code
#[derive(Clone)]
struct RelationFieldInfo {
    /// Field name in the generated struct
    field_name: syn::Ident,
    /// Relation type: "HasOne", "HasMany", or "BelongsTo"
    relation_type: String,
    /// Target Schema path (e.g., crate::models::user::Schema)
    schema_path: TokenStream,
    /// Whether the relation is optional
    is_optional: bool,
    /// If Some, this relation has circular refs and uses an inline type
    /// Contains: (inline_type_name, circular_fields_to_exclude)
    inline_type_info: Option<(syn::Ident, Vec<String>)>,
}

/// Extract the "from" field name from a sea_orm belongs_to attribute.
/// e.g., `#[sea_orm(belongs_to, from = "user_id", to = "id")]` → Some("user_id")
fn extract_belongs_to_from_field(attrs: &[syn::Attribute]) -> Option<String> {
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
fn is_field_optional_in_struct(struct_item: &syn::ItemStruct, field_name: &str) -> bool {
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
fn convert_relation_type_to_schema_with_info(
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

/// Convert a SeaORM relation type to a Schema type.
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
#[allow(dead_code)]
fn convert_relation_type_to_schema(
    ty: &Type,
    field_attrs: &[syn::Attribute],
    parsed_struct: &syn::ItemStruct,
    source_module_path: &[String],
) -> Option<TokenStream> {
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
    // e.g., super::user::Entity with source_module_path = [crate, models, memo]
    //       → [crate, models, user, Schema]
    let absolute_segments: Vec<String> = if !segments.is_empty() && segments[0] == "super" {
        // Count how many `super` segments
        let super_count = segments.iter().take_while(|s| *s == "super").count();

        // Go up `super_count` levels from source module path
        let parent_path_len = source_module_path.len().saturating_sub(super_count);
        let mut abs = source_module_path[..parent_path_len].to_vec();

        // Append remaining segments (after super::), replacing Entity with Schema
        for seg in segments.iter().skip(super_count) {
            if seg == "Entity" {
                abs.push("Schema".to_string());
            } else {
                abs.push(seg.clone());
            }
        }
        abs
    } else if !segments.is_empty() && segments[0] == "crate" {
        // Already absolute path, just replace Entity with Schema
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
        // Relative path without super, assume same module level
        // Prepend source module's parent path
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
            // HasOne → Always Option<Box<Schema>>
            Some(quote! { Option<Box<#schema_path>> })
        }
        "HasMany" => {
            // HasMany → Vec<Schema>
            Some(quote! { Vec<#schema_path> })
        }
        "BelongsTo" => {
            // BelongsTo → Check if "from" field is optional
            if let Some(from_field) = extract_belongs_to_from_field(field_attrs) {
                if is_field_optional_in_struct(parsed_struct, &from_field) {
                    // from field is Option → relation is optional
                    Some(quote! { Option<Box<#schema_path>> })
                } else {
                    // from field is required → relation is required
                    Some(quote! { Box<#schema_path> })
                }
            } else {
                // Fallback: treat as optional if we can't determine
                Some(quote! { Option<Box<#schema_path>> })
            }
        }
        _ => None,
    }
}

/// Generate Schema construction code with field filtering
fn generate_filtered_schema(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
    schema_storage: &[StructMetadata],
) -> Result<TokenStream, syn::Error> {
    let rename_all = extract_rename_all(&struct_item.attrs);

    // Build known_schemas and struct_definitions for type resolution
    let known_schemas: std::collections::HashMap<String, String> = schema_storage
        .iter()
        .map(|s| (s.name.clone(), s.definition.clone()))
        .collect();
    let struct_definitions = known_schemas.clone();

    let mut property_tokens = Vec::new();
    let mut required_fields = Vec::new();

    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            // Skip if serde(skip)
            if extract_skip(&field.attrs) {
                continue;
            }

            let rust_field_name = field
                .ident
                .as_ref()
                .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                .unwrap_or_else(|| "unknown".to_string());

            // Apply rename
            let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                renamed
            } else {
                rename_field(&rust_field_name, rename_all.as_deref())
            };

            // Apply omit filter (check both rust name and json name)
            if !omit_set.is_empty()
                && (omit_set.contains(&rust_field_name) || omit_set.contains(&field_name))
            {
                continue;
            }

            // Apply pick filter (check both rust name and json name)
            if !pick_set.is_empty()
                && !pick_set.contains(&rust_field_name)
                && !pick_set.contains(&field_name)
            {
                continue;
            }

            let field_type = &field.ty;

            // Generate schema for field type
            let schema_ref =
                parse_type_to_schema_ref(field_type, &known_schemas, &struct_definitions);
            let schema_ref_tokens = schema_ref_to_tokens(&schema_ref);

            property_tokens.push(quote! {
                properties.insert(#field_name.to_string(), #schema_ref_tokens);
            });

            // Check if field is required (not Option, no default, no skip_serializing_if)
            let has_default = extract_default(&field.attrs).is_some();
            let has_skip_serializing_if = extract_skip_serializing_if(&field.attrs);
            let is_optional = is_option_type(field_type);

            if !is_optional && !has_default && !has_skip_serializing_if {
                required_fields.push(field_name.clone());
            }
        }
    }

    let required_tokens = if required_fields.is_empty() {
        quote! { None }
    } else {
        let required_strs: Vec<&str> = required_fields.iter().map(|s| s.as_str()).collect();
        quote! { Some(vec![#(#required_strs.to_string()),*]) }
    };

    Ok(quote! {
        {
            let mut properties = std::collections::BTreeMap::new();
            #(#property_tokens)*
            vespera::schema::Schema {
                schema_type: Some(vespera::schema::SchemaType::Object),
                properties: if properties.is_empty() { None } else { Some(properties) },
                required: #required_tokens,
                ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
            }
        }
    })
}

/// Check if a type is Option<T>
fn is_option_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .first()
            .map(|s| s.ident == "Option")
            .unwrap_or(false),
        _ => false,
    }
}

/// Convert SchemaRef to TokenStream for code generation
fn schema_ref_to_tokens(schema_ref: &SchemaRef) -> TokenStream {
    match schema_ref {
        SchemaRef::Ref(reference) => {
            let ref_path = &reference.ref_path;
            quote! {
                vespera::schema::SchemaRef::Ref(vespera::schema::Reference::new(#ref_path.to_string()))
            }
        }
        SchemaRef::Inline(schema) => {
            let schema_tokens = schema_to_tokens(schema);
            quote! {
                vespera::schema::SchemaRef::Inline(Box::new(#schema_tokens))
            }
        }
    }
}

/// Convert Schema to TokenStream for code generation
fn schema_to_tokens(schema: &Schema) -> TokenStream {
    let schema_type_tokens = match &schema.schema_type {
        Some(SchemaType::String) => quote! { Some(vespera::schema::SchemaType::String) },
        Some(SchemaType::Number) => quote! { Some(vespera::schema::SchemaType::Number) },
        Some(SchemaType::Integer) => quote! { Some(vespera::schema::SchemaType::Integer) },
        Some(SchemaType::Boolean) => quote! { Some(vespera::schema::SchemaType::Boolean) },
        Some(SchemaType::Array) => quote! { Some(vespera::schema::SchemaType::Array) },
        Some(SchemaType::Object) => quote! { Some(vespera::schema::SchemaType::Object) },
        Some(SchemaType::Null) => quote! { Some(vespera::schema::SchemaType::Null) },
        None => quote! { None },
    };

    let format_tokens = match &schema.format {
        Some(f) => quote! { Some(#f.to_string()) },
        None => quote! { None },
    };

    let nullable_tokens = match schema.nullable {
        Some(true) => quote! { Some(true) },
        Some(false) => quote! { Some(false) },
        None => quote! { None },
    };

    let ref_path_tokens = match &schema.ref_path {
        Some(rp) => quote! { Some(#rp.to_string()) },
        None => quote! { None },
    };

    let items_tokens = match &schema.items {
        Some(items) => {
            let inner = schema_ref_to_tokens(items);
            quote! { Some(Box::new(#inner)) }
        }
        None => quote! { None },
    };

    let properties_tokens = match &schema.properties {
        Some(props) => {
            let entries: Vec<_> = props
                .iter()
                .map(|(k, v)| {
                    let v_tokens = schema_ref_to_tokens(v);
                    quote! { (#k.to_string(), #v_tokens) }
                })
                .collect();
            quote! {
                Some({
                    let mut map = std::collections::BTreeMap::new();
                    #(map.insert(#entries.0, #entries.1);)*
                    map
                })
            }
        }
        None => quote! { None },
    };

    let required_tokens = match &schema.required {
        Some(req) => {
            let req_strs: Vec<_> = req.iter().map(|s| s.as_str()).collect();
            quote! { Some(vec![#(#req_strs.to_string()),*]) }
        }
        None => quote! { None },
    };

    quote! {
        vespera::schema::Schema {
            ref_path: #ref_path_tokens,
            schema_type: #schema_type_tokens,
            format: #format_tokens,
            nullable: #nullable_tokens,
            items: #items_tokens,
            properties: #properties_tokens,
            required: #required_tokens,
            ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
        }
    }
}

// ============================================================================
// schema_type! macro - Generate new struct types from existing types
// ============================================================================

/// Try to find a struct definition from a module path by reading source files.
///
/// This allows schema_type! to work with structs defined in other files, like:
/// ```ignore
/// // In src/routes/memos.rs
/// schema_type!(CreateMemoRequest from models::memo::Model, pick = ["title", "content"]);
/// ```
///
/// The function will:
/// 1. Parse the path (e.g., `models::memo::Model` or `crate::models::memo::Model`)
/// 2. Convert to file path (e.g., `src/models/memo.rs`)
/// 3. Read and parse the file to find the struct definition
///
/// For simple names (e.g., just `Model` without module path), it will scan all `.rs`
/// files in `src/` to find the struct. This supports same-file usage like:
/// ```ignore
/// pub struct Model { ... }
/// vespera::schema_type!(Schema from Model, name = "UserSchema");
/// ```
///
/// The `schema_name_hint` is used to disambiguate when multiple structs with the same
/// name exist. For example, with `name = "UserSchema"`, it will prefer `user.rs`.
///
/// Returns `(StructMetadata, Vec<String>)` where the Vec is the module path.
/// For qualified paths, this is extracted from the type itself.
/// For simple names, it's inferred from the file location.
fn find_struct_from_path(
    ty: &Type,
    schema_name_hint: Option<&str>,
) -> Option<(StructMetadata, Vec<String>)> {
    // Get CARGO_MANIFEST_DIR to locate src folder
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Extract path segments from the type
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return None,
    };

    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();

    if segments.is_empty() {
        return None;
    }

    // The last segment is the struct name
    let struct_name = segments.last()?.clone();

    // Build possible file paths from the module path
    // e.g., models::memo::Model -> src/models/memo.rs or src/models/memo/mod.rs
    // e.g., crate::models::memo::Model -> src/models/memo.rs
    let module_segments: Vec<&str> = segments[..segments.len() - 1]
        .iter()
        .filter(|s| *s != "crate" && *s != "self" && *s != "super")
        .map(|s| s.as_str())
        .collect();

    // If no module path (simple name like `Model`), scan all files with schema_name hint
    if module_segments.is_empty() {
        return find_struct_by_name_in_all_files(&src_dir, &struct_name, schema_name_hint);
    }

    // For qualified paths, the module path is extracted from the type itself
    // e.g., crate::models::memo::Model → ["crate", "models", "memo"]
    let type_module_path: Vec<String> = segments[..segments.len() - 1].to_vec();

    // Try different file path patterns
    let file_paths = vec![
        src_dir.join(format!("{}.rs", module_segments.join("/"))),
        src_dir.join(format!("{}/mod.rs", module_segments.join("/"))),
    ];

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&file_path).ok()?;
        let file_ast = syn::parse_file(&content).ok()?;

        // Look for the struct in the file
        for item in &file_ast.items {
            match item {
                syn::Item::Struct(struct_item) if struct_item.ident == struct_name => {
                    return Some((
                        StructMetadata::new_model(
                            struct_name.clone(),
                            quote::quote!(#struct_item).to_string(),
                        ),
                        type_module_path,
                    ));
                }
                _ => continue,
            }
        }
    }

    None
}

/// Find a struct by name by scanning all `.rs` files in the src directory.
///
/// This is used as a fallback when the type path doesn't include module information
/// (e.g., just `Model` instead of `crate::models::user::Model`).
///
/// Resolution strategy:
/// 1. If exactly one struct with the name exists → use it
/// 2. If multiple exist and schema_name_hint is provided (e.g., "UserSchema"):
///    → Prefer file whose name contains the hint prefix (e.g., "user.rs" for "UserSchema")
/// 3. Otherwise → return None (ambiguous)
///
/// The `schema_name_hint` is the custom schema name (e.g., "UserSchema", "MemoSchema")
/// which often contains a hint about the module name.
///
/// Returns `(StructMetadata, Vec<String>)` where the Vec is the inferred module path
/// from the file location (e.g., `["crate", "models", "user"]`).
fn find_struct_by_name_in_all_files(
    src_dir: &Path,
    struct_name: &str,
    schema_name_hint: Option<&str>,
) -> Option<(StructMetadata, Vec<String>)> {
    // Collect all .rs files recursively
    let mut rs_files = Vec::new();
    collect_rs_files_recursive(src_dir, &mut rs_files);

    // Store: (file_path, struct_metadata)
    let mut found_structs: Vec<(std::path::PathBuf, StructMetadata)> = Vec::new();

    for file_path in rs_files {
        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let file_ast = match syn::parse_file(&content) {
            Ok(ast) => ast,
            Err(_) => continue,
        };

        // Look for the struct in the file
        for item in &file_ast.items {
            if let syn::Item::Struct(struct_item) = item
                && struct_item.ident == struct_name
            {
                found_structs.push((
                    file_path.clone(),
                    StructMetadata::new_model(
                        struct_name.to_string(),
                        quote::quote!(#struct_item).to_string(),
                    ),
                ));
            }
        }
    }

    match found_structs.len() {
        0 => None,
        1 => {
            let (path, metadata) = found_structs.remove(0);
            let module_path = file_path_to_module_path(&path, src_dir);
            Some((metadata, module_path))
        }
        _ => {
            // Multiple structs with same name - try to disambiguate using schema_name_hint
            if let Some(hint) = schema_name_hint {
                // Extract prefix from schema name (e.g., "UserSchema" -> "user", "MemoSchema" -> "memo")
                let hint_lower = hint.to_lowercase();
                let prefix = hint_lower
                    .strip_suffix("schema")
                    .or_else(|| hint_lower.strip_suffix("response"))
                    .or_else(|| hint_lower.strip_suffix("request"))
                    .unwrap_or(&hint_lower);

                // Find files whose name contains the prefix
                let matching: Vec<_> = found_structs
                    .into_iter()
                    .filter(|(path, _)| {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .is_some_and(|name| name.to_lowercase().contains(prefix))
                    })
                    .collect();

                if matching.len() == 1 {
                    let (path, metadata) = matching.into_iter().next().unwrap();
                    let module_path = file_path_to_module_path(&path, src_dir);
                    return Some((metadata, module_path));
                }
            }

            // Still ambiguous
            None
        }
    }
}

/// Recursively collect all `.rs` files in a directory.
fn collect_rs_files_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_recursive(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

/// Derive module path from a file path relative to src directory.
///
/// Examples:
/// - `src/models/user.rs` → `["crate", "models", "user"]`
/// - `src/models/user/mod.rs` → `["crate", "models", "user"]`
/// - `src/lib.rs` → `["crate"]`
fn file_path_to_module_path(file_path: &Path, src_dir: &Path) -> Vec<String> {
    let relative = match file_path.strip_prefix(src_dir) {
        Ok(r) => r,
        Err(_) => return vec!["crate".to_string()],
    };

    let mut segments = vec!["crate".to_string()];

    for component in relative.components() {
        if let std::path::Component::Normal(os_str) = component
            && let Some(s) = os_str.to_str()
        {
            // Handle .rs extension
            if let Some(name) = s.strip_suffix(".rs") {
                // Skip mod.rs and lib.rs - they don't add a segment
                if name != "mod" && name != "lib" {
                    segments.push(name.to_string());
                }
            } else {
                // Directory name
                segments.push(s.to_string());
            }
        }
    }

    segments
}

/// Find struct definition from a schema path string (e.g., "crate::models::user::Schema").
///
/// Similar to `find_struct_from_path` but takes a string path instead of syn::Type.
fn find_struct_from_schema_path(path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the path string into segments
    let segments: Vec<&str> = path_str.split("::").filter(|s| !s.is_empty()).collect();

    if segments.is_empty() {
        return None;
    }

    // The last segment is the struct name
    let struct_name = segments.last()?.to_string();

    // Build possible file paths from the module path
    // e.g., crate::models::user::Schema -> src/models/user.rs
    let module_segments: Vec<&str> = segments[..segments.len() - 1]
        .iter()
        .filter(|s| **s != "crate" && **s != "self" && **s != "super")
        .copied()
        .collect();

    if module_segments.is_empty() {
        return None;
    }

    // Try different file path patterns
    let file_paths = vec![
        src_dir.join(format!("{}.rs", module_segments.join("/"))),
        src_dir.join(format!("{}/mod.rs", module_segments.join("/"))),
    ];

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&file_path).ok()?;
        let file_ast = syn::parse_file(&content).ok()?;

        // Look for the struct in the file
        for item in &file_ast.items {
            match item {
                syn::Item::Struct(struct_item) if struct_item.ident == struct_name => {
                    return Some(StructMetadata::new_model(
                        struct_name.clone(),
                        quote::quote!(#struct_item).to_string(),
                    ));
                }
                _ => continue,
            }
        }
    }

    None
}

/// Find the Model definition from a Schema path.
/// Converts "crate::models::user::Schema" -> finds Model in src/models/user.rs
fn find_model_from_schema_path(schema_path_str: &str) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Parse the path string and convert Schema path to module path
    // e.g., "crate :: models :: user :: Schema" -> ["crate", "models", "user"]
    let segments: Vec<&str> = schema_path_str
        .split("::")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "Schema")
        .collect();

    if segments.is_empty() {
        return None;
    }

    // Build possible file paths from the module path
    let module_segments: Vec<&str> = segments
        .iter()
        .filter(|s| **s != "crate" && **s != "self" && **s != "super")
        .copied()
        .collect();

    if module_segments.is_empty() {
        return None;
    }

    // Try different file path patterns
    let file_paths = vec![
        src_dir.join(format!("{}.rs", module_segments.join("/"))),
        src_dir.join(format!("{}/mod.rs", module_segments.join("/"))),
    ];

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&file_path).ok()?;
        let file_ast = syn::parse_file(&content).ok()?;

        // Look for Model struct in the file
        for item in &file_ast.items {
            if let syn::Item::Struct(struct_item) = item
                && struct_item.ident == "Model"
            {
                return Some(StructMetadata::new_model(
                    "Model".to_string(),
                    quote::quote!(#struct_item).to_string(),
                ));
            }
        }
    }

    None
}

/// Information about an inline relation type to generate
struct InlineRelationType {
    /// Name of the inline type (e.g., MemoResponseRel_User)
    type_name: syn::Ident,
    /// Fields to include (excluding circular references)
    fields: Vec<InlineField>,
    /// The effective rename_all strategy
    rename_all: String,
}

/// A field in an inline relation type
struct InlineField {
    name: syn::Ident,
    ty: TokenStream,
    attrs: Vec<syn::Attribute>,
}

/// Generate inline relation type definition for circular references.
///
/// When `MemoSchema.user` would reference `UserSchema` which has `memos: Vec<MemoSchema>`,
/// we instead generate an inline type `MemoSchema_User` that excludes the `memos` field.
///
/// The `schema_name_override` parameter allows using a custom schema name (e.g., "MemoSchema")
/// instead of the Rust struct name (e.g., "Schema") for the inline type name.
fn generate_inline_relation_type(
    parent_type_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    source_module_path: &[String],
    schema_name_override: Option<&str>,
) -> Option<InlineRelationType> {
    // Find the target model definition
    let schema_path_str = rel_info.schema_path.to_string();
    let model_metadata = find_model_from_schema_path(&schema_path_str)?;
    let model_def = &model_metadata.definition;

    // Parse the model struct
    let parsed_model: syn::ItemStruct = syn::parse_str(model_def).ok()?;

    // Detect circular fields
    let circular_fields = detect_circular_fields("", source_module_path, model_def);

    // If no circular fields, no need for inline type
    if circular_fields.is_empty() {
        return None;
    }

    // Get rename_all from model (or default to camelCase)
    let rename_all =
        extract_rename_all(&parsed_model.attrs).unwrap_or_else(|| "camelCase".to_string());

    // Generate inline type name: {SchemaName}_{Field}
    // Use custom schema name if provided, otherwise use the Rust struct name
    let parent_name = match schema_name_override {
        Some(name) => name.to_string(),
        None => parent_type_name.to_string(),
    };
    let field_name_pascal = capitalize_first(&rel_info.field_name.to_string());
    let inline_type_name = syn::Ident::new(
        &format!("{}_{}", parent_name, field_name_pascal),
        proc_macro2::Span::call_site(),
    );

    // Collect fields, excluding circular ones and relation types
    let mut fields = Vec::new();
    if let syn::Fields::Named(fields_named) = &parsed_model.fields {
        for field in &fields_named.named {
            let field_ident = field.ident.as_ref()?;
            let field_name_str = field_ident.to_string();

            // Skip circular fields
            if circular_fields.contains(&field_name_str) {
                continue;
            }

            // Skip relation types (HasOne, HasMany, BelongsTo)
            if is_seaorm_relation_type(&field.ty) {
                continue;
            }

            // Skip fields with serde(skip)
            if extract_skip(&field.attrs) {
                continue;
            }

            // Keep only serde attributes
            let serde_attrs: Vec<syn::Attribute> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("serde"))
                .cloned()
                .collect();

            let field_ty = &field.ty;
            fields.push(InlineField {
                name: field_ident.clone(),
                ty: quote::quote!(#field_ty),
                attrs: serde_attrs,
            });
        }
    }

    Some(InlineRelationType {
        type_name: inline_type_name,
        fields,
        rename_all,
    })
}

/// Generate inline relation type for HasMany with ALL relations stripped.
///
/// When a HasMany relation is explicitly picked, the nested items should have
/// NO relation fields at all (not even FK relations). This prevents infinite
/// nesting and keeps the schema simple.
///
/// Example: If UserSchema picks "memos", each memo in the list will have
/// id, user_id, title, content, etc. but NO user or comments relations.
fn generate_inline_relation_type_no_relations(
    parent_type_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    schema_name_override: Option<&str>,
) -> Option<InlineRelationType> {
    // Find the target model definition
    let schema_path_str = rel_info.schema_path.to_string();
    let model_metadata = find_model_from_schema_path(&schema_path_str)?;
    let model_def = &model_metadata.definition;

    // Parse the model struct
    let parsed_model: syn::ItemStruct = syn::parse_str(model_def).ok()?;

    // Get rename_all from model (or default to camelCase)
    let rename_all =
        extract_rename_all(&parsed_model.attrs).unwrap_or_else(|| "camelCase".to_string());

    // Generate inline type name: {SchemaName}_{Field}
    let parent_name = match schema_name_override {
        Some(name) => name.to_string(),
        None => parent_type_name.to_string(),
    };
    let field_name_pascal = capitalize_first(&rel_info.field_name.to_string());
    let inline_type_name = syn::Ident::new(
        &format!("{}_{}", parent_name, field_name_pascal),
        proc_macro2::Span::call_site(),
    );

    // Collect fields, excluding ALL relation types
    let mut fields = Vec::new();
    if let syn::Fields::Named(fields_named) = &parsed_model.fields {
        for field in &fields_named.named {
            let field_ident = field.ident.as_ref()?;

            // Skip ALL relation types (HasOne, HasMany, BelongsTo)
            if is_seaorm_relation_type(&field.ty) {
                continue;
            }

            // Skip fields with serde(skip)
            if extract_skip(&field.attrs) {
                continue;
            }

            // Keep only serde attributes
            let serde_attrs: Vec<syn::Attribute> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("serde"))
                .cloned()
                .collect();

            let field_ty = &field.ty;
            fields.push(InlineField {
                name: field_ident.clone(),
                ty: quote::quote!(#field_ty),
                attrs: serde_attrs,
            });
        }
    }

    Some(InlineRelationType {
        type_name: inline_type_name,
        fields,
        rename_all,
    })
}

/// Generate the struct definition TokenStream for an inline relation type
fn generate_inline_type_definition(inline_type: &InlineRelationType) -> TokenStream {
    let type_name = &inline_type.type_name;
    let rename_all = &inline_type.rename_all;

    let field_tokens: Vec<TokenStream> = inline_type
        .fields
        .iter()
        .map(|f| {
            let name = &f.name;
            let ty = &f.ty;
            let attrs = &f.attrs;
            quote! {
                #(#attrs)*
                pub #name: #ty
            }
        })
        .collect();

    quote! {
        #[derive(Clone, serde::Serialize, serde::Deserialize, vespera::Schema)]
        #[serde(rename_all = #rename_all)]
        pub struct #type_name {
            #(#field_tokens),*
        }
    }
}

/// Input for the schema_type! macro
///
/// Syntax: `schema_type!(NewTypeName from SourceType, pick = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, omit = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, rename = [("old", "new")])`
/// Or:     `schema_type!(NewTypeName from SourceType, add = [("field": Type)])`
/// Or:     `schema_type!(NewTypeName from SourceType, ignore)` - skip Schema derive
/// Or:     `schema_type!(NewTypeName from SourceType, name = "CustomName")` - custom OpenAPI name
/// Or:     `schema_type!(NewTypeName from SourceType, rename_all = "camelCase")` - serde rename_all
pub struct SchemaTypeInput {
    /// The new type name to generate
    pub new_type: Ident,
    /// The source type to derive from
    pub source_type: Type,
    /// Fields to omit from the new type
    pub omit: Option<Vec<String>>,
    /// Fields to pick (include only these fields)
    pub pick: Option<Vec<String>>,
    /// Field renames: (source_field_name, new_field_name)
    pub rename: Option<Vec<(String, String)>>,
    /// New fields to add: (field_name, field_type)
    pub add: Option<Vec<(String, Type)>>,
    /// Whether to derive Clone (default: true)
    pub derive_clone: bool,
    /// Fields to wrap in `Option<T>` for partial updates.
    ///
    /// - `partial` (bare) = all fields become `Option<T>`
    /// - `partial = ["field1", "field2"]` = only listed fields become `Option<T>`
    /// - Fields already `Option<T>` are left unchanged.
    pub partial: Option<PartialMode>,
    /// Whether to skip deriving the Schema trait (default: false)
    /// Use `ignore` keyword to set this to true.
    pub ignore_schema: bool,
    /// Custom OpenAPI schema name (overrides Rust struct name)
    /// Use `name = "CustomName"` to set this.
    pub schema_name: Option<String>,
    /// Serde rename_all strategy (e.g., "camelCase", "snake_case", "PascalCase")
    /// If not specified, defaults to "camelCase" when source has no rename_all
    pub rename_all: Option<String>,
}

/// Mode for the `partial` keyword in schema_type!
#[derive(Clone, Debug)]
pub enum PartialMode {
    /// All fields become Option<T>
    All,
    /// Only listed fields become Option<T>
    Fields(Vec<String>),
}

/// Helper struct to parse an add field: ("field_name": Type)
struct AddField {
    name: String,
    ty: Type,
}

impl Parse for AddField {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let name: LitStr = content.parse()?;
        content.parse::<Token![:]>()?;
        let ty: Type = content.parse()?;
        Ok(AddField {
            name: name.value(),
            ty,
        })
    }
}

/// Helper struct to parse a rename pair: ("old_name", "new_name")
struct RenamePair {
    from: String,
    to: String,
}

impl Parse for RenamePair {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let from: LitStr = content.parse()?;
        content.parse::<Token![,]>()?;
        let to: LitStr = content.parse()?;
        Ok(RenamePair {
            from: from.value(),
            to: to.value(),
        })
    }
}

impl Parse for SchemaTypeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse new type name
        let new_type: Ident = input.parse()?;

        // Parse "from" keyword
        let from_ident: Ident = input.parse()?;
        if from_ident != "from" {
            return Err(syn::Error::new(
                from_ident.span(),
                format!("expected `from`, found `{}`", from_ident),
            ));
        }

        // Parse source type
        let source_type: Type = input.parse()?;

        let mut omit = None;
        let mut pick = None;
        let mut rename = None;
        let mut add = None;
        let mut derive_clone = true;
        let mut partial = None;
        let mut ignore_schema = false;
        let mut schema_name = None;
        let mut rename_all = None;

        // Parse optional parameters
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "omit" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    omit = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                "pick" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    pick = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                "rename" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let pairs: Punctuated<RenamePair, Token![,]> =
                        content.parse_terminated(RenamePair::parse, Token![,])?;
                    rename = Some(pairs.into_iter().map(|p| (p.from, p.to)).collect());
                }
                "add" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<AddField, Token![,]> =
                        content.parse_terminated(AddField::parse, Token![,])?;
                    add = Some(fields.into_iter().map(|f| (f.name, f.ty)).collect());
                }
                "clone" => {
                    input.parse::<Token![=]>()?;
                    let value: syn::LitBool = input.parse()?;
                    derive_clone = value.value();
                }
                "partial" => {
                    if input.peek(Token![=]) {
                        // partial = ["field1", "field2"]
                        input.parse::<Token![=]>()?;
                        let content;
                        let _ = bracketed!(content in input);
                        let fields: Punctuated<LitStr, Token![,]> =
                            content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                        partial = Some(PartialMode::Fields(
                            fields.into_iter().map(|s| s.value()).collect(),
                        ));
                    } else {
                        // bare `partial` — all fields
                        partial = Some(PartialMode::All);
                    }
                }
                "ignore" => {
                    // bare `ignore` — skip Schema derive
                    ignore_schema = true;
                }
                "name" => {
                    // name = "CustomSchemaName" — custom OpenAPI schema name
                    input.parse::<Token![=]>()?;
                    let name_lit: LitStr = input.parse()?;
                    schema_name = Some(name_lit.value());
                }
                "rename_all" => {
                    // rename_all = "camelCase" — serde rename_all strategy
                    // Validation is delegated to serde at compile time
                    input.parse::<Token![=]>()?;
                    let rename_all_lit: LitStr = input.parse()?;
                    rename_all = Some(rename_all_lit.value());
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown parameter: `{}`. Expected `omit`, `pick`, `rename`, `add`, `clone`, `partial`, `ignore`, `name`, or `rename_all`",
                            ident_str
                        ),
                    ));
                }
            }
        }

        // Validate: can't use both omit and pick
        if omit.is_some() && pick.is_some() {
            return Err(syn::Error::new(
                input.span(),
                "cannot use both `omit` and `pick` in the same schema_type! invocation",
            ));
        }

        Ok(SchemaTypeInput {
            new_type,
            source_type,
            omit,
            pick,
            rename,
            add,
            derive_clone,
            partial,
            ignore_schema,
            schema_name,
            rename_all,
        })
    }
}

/// Extract the module path from a type (excluding the type name itself).
/// e.g., `crate::models::memo::Model` → ["crate", "models", "memo"]
fn extract_module_path(ty: &Type) -> Vec<String> {
    match ty {
        Type::Path(type_path) => {
            let segments: Vec<String> = type_path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            // Return all but the last segment (which is the type name)
            if segments.len() > 1 {
                segments[..segments.len() - 1].to_vec()
            } else {
                vec![]
            }
        }
        _ => vec![],
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
fn detect_circular_fields(
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
            let ty_str = quote::quote!(#field_ty).to_string();

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
fn has_fk_relations(model_def: &str) -> bool {
    let Ok(parsed) = syn::parse_str::<syn::ItemStruct>(model_def) else {
        return false;
    };

    if let syn::Fields::Named(fields_named) = &parsed.fields {
        for field in &fields_named.named {
            let field_ty = &field.ty;
            let ty_str = quote::quote!(#field_ty).to_string().replace(' ', "");

            // Check for BelongsTo or HasOne patterns
            if ty_str.contains("HasOne<") || ty_str.contains("BelongsTo<") {
                return true;
            }
        }
    }

    false
}

/// Capitalize the first letter of a string.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
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
fn generate_inline_struct_construction(
    schema_path: &TokenStream,
    related_schema_def: &str,
    circular_fields: &[String],
    var_name: &str,
) -> TokenStream {
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
fn generate_inline_type_construction(
    inline_type_name: &syn::Ident,
    included_fields: &[String],
    related_model_def: &str,
    var_name: &str,
) -> TokenStream {
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

/// Check if a circular relation field in the related schema is required (Box<T>) or optional (Option<Box<T>>).
///
/// Returns true if the circular relation is required and needs a parent stub.
fn is_circular_relation_required(related_model_def: &str, circular_field_name: &str) -> bool {
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
            let ty_str = quote::quote!(#field.ty).to_string().replace(' ', "");
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
fn generate_default_for_relation_field(
    ty: &Type,
    field_ident: &syn::Ident,
    field_attrs: &[syn::Attribute],
    all_fields: &syn::FieldsNamed,
) -> TokenStream {
    let ty_str = quote::quote!(#ty).to_string().replace(' ', "");

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

/// Generate `from_model` impl for SeaORM Model WITH relations (async version).
///
/// When circular references are detected, generates inline struct construction
/// that excludes circular fields (sets them to default values).
///
/// ```ignore
/// impl NewType {
///     pub async fn from_model(
///         model: SourceType,
///         db: &sea_orm::DatabaseConnection,
///     ) -> Result<Self, sea_orm::DbErr> {
///         // Load related entities
///         let user = model.find_related(user::Entity).one(db).await?;
///         let tags = model.find_related(tag::Entity).all(db).await?;
///
///         Ok(Self {
///             id: model.id,
///             // Inline construction with circular field defaulted:
///             user: user.map(|r| Box::new(user::Schema { id: r.id, memos: vec![], ... })),
///             tags: tags.into_iter().map(|r| tag::Schema { ... }).collect(),
///         })
///     }
/// }
/// ```
fn generate_from_model_with_relations(
    new_type_name: &syn::Ident,
    source_type: &Type,
    field_mappings: &[(syn::Ident, syn::Ident, bool, bool)],
    relation_fields: &[RelationFieldInfo],
    source_module_path: &[String],
    _schema_storage: &[StructMetadata],
) -> TokenStream {
    // Build relation loading statements
    let relation_loads: Vec<TokenStream> = relation_fields
        .iter()
        .map(|rel| {
            let field_name = &rel.field_name;
            let entity_path =
                build_entity_path_from_schema_path(&rel.schema_path, source_module_path);

            match rel.relation_type.as_str() {
                "HasOne" | "BelongsTo" => {
                    // Load single related entity
                    quote! {
                        let #field_name = model.find_related(#entity_path).one(db).await?;
                    }
                }
                "HasMany" => {
                    // Load multiple related entities
                    quote! {
                        let #field_name = model.find_related(#entity_path).all(db).await?;
                    }
                }
                _ => quote! {},
            }
        })
        .collect();

    // Check if we need a parent stub for HasMany relations with required circular back-refs
    // This is needed when: UserSchema.memos has MemoSchema which has required user: Box<UserSchema>
    // BUT: If the relation uses an inline type (which excludes circular fields), we don't need a parent stub
    let needs_parent_stub = relation_fields.iter().any(|rel| {
        if rel.relation_type != "HasMany" {
            return false;
        }
        // If using inline type, circular fields are excluded, so no parent stub needed
        if rel.inline_type_info.is_some() {
            return false;
        }
        let schema_path_str = rel.schema_path.to_string().replace(' ', "");
        let model_path_str = schema_path_str.replace("::Schema", "::Model");
        let related_model = find_struct_from_schema_path(&model_path_str);

        if let Some(ref model) = related_model {
            let circular_fields = detect_circular_fields(
                new_type_name.to_string().as_str(),
                source_module_path,
                &model.definition,
            );
            // Check if any circular field is a required relation
            circular_fields
                .iter()
                .any(|cf| is_circular_relation_required(&model.definition, cf))
        } else {
            false
        }
    });

    // Generate parent stub field assignments (non-relation fields from model)
    let parent_stub_fields: Vec<TokenStream> = if needs_parent_stub {
        field_mappings
            .iter()
            .map(|(new_ident, source_ident, _wrapped, is_relation)| {
                if *is_relation {
                    // For relation fields in stub, use defaults
                    if let Some(rel) = relation_fields
                        .iter()
                        .find(|r| &r.field_name == source_ident)
                    {
                        match rel.relation_type.as_str() {
                            "HasMany" => quote! { #new_ident: vec![] },
                            _ if rel.is_optional => quote! { #new_ident: None },
                            // Required single relations in parent stub - this shouldn't happen
                            // as we're creating stub to break circular ref
                            _ => quote! { #new_ident: None },
                        }
                    } else {
                        quote! { #new_ident: Default::default() }
                    }
                } else {
                    // Regular field - clone from model
                    quote! { #new_ident: model.#source_ident.clone() }
                }
            })
            .collect()
    } else {
        vec![]
    };

    // Build field assignments
    // For relation fields, check for circular references and use inline construction if needed
    let field_assignments: Vec<TokenStream> = field_mappings
        .iter()
        .map(|(new_ident, source_ident, wrapped, is_relation)| {
            if *is_relation {
                // Find the relation info for this field
                if let Some(rel) = relation_fields.iter().find(|r| &r.field_name == source_ident) {
                    let schema_path = &rel.schema_path;

                    // Try to find the related MODEL definition to check for circular refs
                    // The schema_path is like "crate::models::user::Schema", but the actual
                    // struct is "Model" in the same module. We need to look up the Model
                    // to see if it has relations pointing back to us.
                    let schema_path_str = schema_path.to_string().replace(' ', "");

                    // Convert schema path to model path: Schema -> Model
                    let model_path_str = schema_path_str.replace("::Schema", "::Model");

                    // Try to find the related Model definition from file
                    let related_model_from_file = find_struct_from_schema_path(&model_path_str);

                    // Get the definition string
                    let related_def_str = related_model_from_file
                        .as_ref()
                        .map(|s| s.definition.as_str())
                        .unwrap_or("");

                    // Check for circular references
                    // The source module path tells us what module we're in (e.g., ["crate", "models", "memo"])
                    // We need to check if the related model has any relation fields pointing back to our module
                    let circular_fields = detect_circular_fields(
                        new_type_name.to_string().as_str(),
                        source_module_path,
                        related_def_str,
                    );

                    let has_circular = !circular_fields.is_empty();

                    // Check if we have inline type info - if so, use the inline type
                    // instead of the original schema path
                    if let Some((ref inline_type_name, ref included_fields)) = rel.inline_type_info {
                        // Use inline type construction
                        let inline_construct = generate_inline_type_construction(
                            inline_type_name,
                            included_fields,
                            related_def_str,
                            "r",
                        );

                        match rel.relation_type.as_str() {
                            "HasOne" | "BelongsTo" => {
                                if rel.is_optional {
                                    quote! {
                                        #new_ident: #source_ident.map(|r| Box::new(#inline_construct))
                                    }
                                } else {
                                    quote! {
                                        #new_ident: Box::new({
                                            let r = #source_ident.ok_or_else(|| sea_orm::DbErr::RecordNotFound(
                                                format!("Required relation '{}' not found", stringify!(#source_ident))
                                            ))?;
                                            #inline_construct
                                        })
                                    }
                                }
                            }
                            "HasMany" => {
                                quote! {
                                    #new_ident: #source_ident.into_iter().map(|r| #inline_construct).collect()
                                }
                            }
                            _ => quote! { #new_ident: Default::default() },
                        }
                    } else {
                        // No inline type - use original behavior
                        match rel.relation_type.as_str() {
                            "HasOne" | "BelongsTo" => {
                                if has_circular {
                                    // Use inline construction to break circular ref
                                    let inline_construct = generate_inline_struct_construction(
                                        schema_path,
                                        related_def_str,
                                        &circular_fields,
                                        "r",
                                    );
                                    if rel.is_optional {
                                        quote! {
                                            #new_ident: #source_ident.map(|r| Box::new(#inline_construct))
                                        }
                                    } else {
                                        quote! {
                                            #new_ident: Box::new({
                                                let r = #source_ident.ok_or_else(|| sea_orm::DbErr::RecordNotFound(
                                                    format!("Required relation '{}' not found", stringify!(#source_ident))
                                                ))?;
                                                #inline_construct
                                            })
                                        }
                                    }
                                } else {
                                    // No circular ref - check if target schema has FK relations
                                    let target_has_fk = has_fk_relations(related_def_str);

                                    if target_has_fk {
                                        // Target schema has FK relations → use async from_model()
                                        if rel.is_optional {
                                            quote! {
                                                #new_ident: match #source_ident {
                                                    Some(r) => Some(Box::new(#schema_path::from_model(r, db).await?)),
                                                    None => None,
                                                }
                                            }
                                        } else {
                                            quote! {
                                                #new_ident: Box::new(#schema_path::from_model(
                                                    #source_ident.ok_or_else(|| sea_orm::DbErr::RecordNotFound(
                                                        format!("Required relation '{}' not found", stringify!(#source_ident))
                                                    ))?,
                                                    db,
                                                ).await?)
                                            }
                                        }
                                    } else {
                                        // Target schema has no FK relations → use sync From::from()
                                        if rel.is_optional {
                                            quote! {
                                                #new_ident: #source_ident.map(|r| Box::new(<#schema_path as From<_>>::from(r)))
                                            }
                                        } else {
                                            quote! {
                                                #new_ident: Box::new(<#schema_path as From<_>>::from(
                                                    #source_ident.ok_or_else(|| sea_orm::DbErr::RecordNotFound(
                                                        format!("Required relation '{}' not found", stringify!(#source_ident))
                                                    ))?
                                                ))
                                            }
                                        }
                                    }
                                }
                            }
                            "HasMany" => {
                                // HasMany is excluded by default, so this branch is only hit
                                // when explicitly picked. Use inline construction (no relations).
                                if has_circular {
                                    // Use inline construction to break circular ref
                                    let inline_construct = generate_inline_struct_construction(
                                        schema_path,
                                        related_def_str,
                                        &circular_fields,
                                        "r",
                                    );
                                    quote! {
                                        #new_ident: #source_ident.into_iter().map(|r| #inline_construct).collect()
                                    }
                                } else {
                                    // No circular ref - check if target schema has FK relations
                                    let target_has_fk = has_fk_relations(related_def_str);

                                    if target_has_fk {
                                        // Target has FK relations but HasMany doesn't load nested data anyway,
                                        // so we use inline construction (flat fields only)
                                        let inline_construct = generate_inline_struct_construction(
                                            schema_path,
                                            related_def_str,
                                            &[],  // no circular fields to exclude
                                            "r",
                                        );
                                        quote! {
                                            #new_ident: #source_ident.into_iter().map(|r| #inline_construct).collect()
                                        }
                                    } else {
                                        quote! {
                                            #new_ident: #source_ident.into_iter().map(|r| <#schema_path as From<_>>::from(r)).collect()
                                        }
                                    }
                                }
                            }
                            _ => quote! { #new_ident: Default::default() },
                        }
                    }
                } else {
                    quote! { #new_ident: Default::default() }
                }
            } else if *wrapped {
                quote! { #new_ident: Some(model.#source_ident) }
            } else {
                quote! { #new_ident: model.#source_ident }
            }
        })
        .collect();

    // Circular references are now handled automatically via inline construction
    // For HasMany with required circular back-refs, we create a parent stub first

    // Generate parent stub definition if needed
    let parent_stub_def = if needs_parent_stub {
        quote! {
            #[allow(unused_variables)]
            let __parent_stub__ = Self {
                #(#parent_stub_fields),*
            };
        }
    } else {
        quote! {}
    };

    quote! {
        impl #new_type_name {
            pub async fn from_model(
                model: #source_type,
                db: &sea_orm::DatabaseConnection,
            ) -> Result<Self, sea_orm::DbErr> {
                use sea_orm::ModelTrait;

                #(#relation_loads)*

                #parent_stub_def

                Ok(Self {
                    #(#field_assignments),*
                })
            }
        }
    }
}

/// Build Entity path from Schema path.
/// e.g., `crate::models::user::Schema` -> `crate::models::user::Entity`
fn build_entity_path_from_schema_path(
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

/// Generate a new struct type from an existing type with field filtering
///
/// Returns (TokenStream, Option<StructMetadata>) where the metadata is returned
/// when a custom `name` is provided (for direct registration in SCHEMA_STORAGE).
pub fn generate_schema_type_code(
    input: &SchemaTypeInput,
    schema_storage: &[StructMetadata],
) -> Result<(TokenStream, Option<StructMetadata>), syn::Error> {
    // Extract type name from the source Type
    let source_type_name = extract_type_name(&input.source_type)?;

    // Extract the module path for resolving relative paths in relation types
    // This may be empty for simple names like `Model` - will be overridden below if found from file
    let mut source_module_path = extract_module_path(&input.source_type);

    // Find struct definition - lookup order depends on whether path is qualified
    // For qualified paths (crate::models::memo::Model), try file lookup FIRST
    // to avoid name collisions when multiple modules have same struct name (e.g., Model)
    let struct_def_owned: StructMetadata;
    let schema_name_hint = input.schema_name.as_deref();
    let struct_def = if is_qualified_path(&input.source_type) {
        // Qualified path: try file lookup first, then storage
        if let Some((found, module_path)) =
            find_struct_from_path(&input.source_type, schema_name_hint)
        {
            struct_def_owned = found;
            // Use the module path from the file lookup if the extracted one is empty
            if source_module_path.is_empty() {
                source_module_path = module_path;
            }
            &struct_def_owned
        } else if let Some(found) = schema_storage.iter().find(|s| s.name == source_type_name) {
            found
        } else {
            return Err(syn::Error::new_spanned(
                &input.source_type,
                format!(
                    "type `{}` not found. Either:\n\
                     1. Use #[derive(Schema)] in the same file\n\
                     2. Use full module path like `crate::models::memo::Model` to reference a struct from another file",
                    source_type_name
                ),
            ));
        }
    } else {
        // Simple name: try storage first (for same-file structs), then file lookup with schema name hint
        if let Some(found) = schema_storage.iter().find(|s| s.name == source_type_name) {
            found
        } else if let Some((found, module_path)) =
            find_struct_from_path(&input.source_type, schema_name_hint)
        {
            struct_def_owned = found;
            // For simple names, we MUST use the inferred module path from the file location
            // This is crucial for resolving relative paths like `super::user::Entity`
            source_module_path = module_path;
            &struct_def_owned
        } else {
            return Err(syn::Error::new_spanned(
                &input.source_type,
                format!(
                    "type `{}` not found. Either:\n\
                     1. Use #[derive(Schema)] in the same file\n\
                     2. Use full module path like `crate::models::memo::Model` to reference a struct from another file\n\
                     3. If using `name = \"XxxSchema\"`, ensure the file name matches (e.g., xxx.rs)",
                    source_type_name
                ),
            ));
        }
    };

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = syn::parse_str(&struct_def.definition).map_err(|e| {
        syn::Error::new_spanned(
            &input.source_type,
            format!(
                "failed to parse struct definition for `{}`: {}",
                source_type_name, e
            ),
        )
    })?;

    // Extract all field names from source struct for validation
    // Include relation fields since they can be converted to Schema types
    let source_field_names: HashSet<String> =
        if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
            fields_named
                .named
                .iter()
                .filter_map(|f| f.ident.as_ref())
                .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                .collect()
        } else {
            HashSet::new()
        };

    // Validate pick fields exist
    if let Some(ref pick_fields) = input.pick {
        for field in pick_fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    &input.source_type,
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

    // Validate omit fields exist
    if let Some(ref omit_fields) = input.omit {
        for field in omit_fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    &input.source_type,
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

    // Validate rename source fields exist
    if let Some(ref rename_pairs) = input.rename {
        for (from_field, _) in rename_pairs {
            if !source_field_names.contains(from_field) {
                return Err(syn::Error::new_spanned(
                    &input.source_type,
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

    // Validate partial fields exist (when specific fields are listed)
    if let Some(PartialMode::Fields(ref partial_fields)) = input.partial {
        for field in partial_fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    &input.source_type,
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

    // Build omit set (use Rust field names)
    let omit_set: HashSet<String> = input.omit.clone().unwrap_or_default().into_iter().collect();

    // Build pick set (use Rust field names)
    let pick_set: HashSet<String> = input.pick.clone().unwrap_or_default().into_iter().collect();

    // Build partial set
    let partial_all = matches!(input.partial, Some(PartialMode::All));
    let partial_set: HashSet<String> = match &input.partial {
        Some(PartialMode::Fields(fields)) => fields.iter().cloned().collect(),
        _ => HashSet::new(),
    };

    // Build rename map: source_field_name -> new_field_name
    let rename_map: std::collections::HashMap<String, String> = input
        .rename
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Extract serde attributes from source struct, excluding rename_all (we'll handle it separately)
    let serde_attrs_without_rename_all: Vec<_> = parsed_struct
        .attrs
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
        .collect();

    // Determine the rename_all strategy:
    // 1. If input.rename_all is specified, use it
    // 2. Else if source has rename_all, use it
    // 3. Else default to "camelCase"
    let effective_rename_all = if let Some(ref ra) = input.rename_all {
        ra.clone()
    } else {
        // Check source struct for existing rename_all
        extract_rename_all(&parsed_struct.attrs).unwrap_or_else(|| "camelCase".to_string())
    };

    // Check if source is a SeaORM Model
    let is_source_seaorm_model = is_seaorm_model(&parsed_struct);

    // Generate new struct with filtered fields
    let new_type_name = &input.new_type;
    let mut field_tokens = Vec::new();
    // Track field mappings for From impl: (new_field_ident, source_field_ident, wrapped_in_option, is_relation)
    let mut field_mappings: Vec<(syn::Ident, syn::Ident, bool, bool)> = Vec::new();
    // Track relation field info for from_model generation
    let mut relation_fields: Vec<RelationFieldInfo> = Vec::new();
    // Track inline types that need to be generated for circular relations
    let mut inline_type_definitions: Vec<TokenStream> = Vec::new();

    if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
        for field in &fields_named.named {
            let rust_field_name = field
                .ident
                .as_ref()
                .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                .unwrap_or_else(|| "unknown".to_string());

            // Apply omit filter
            if !omit_set.is_empty() && omit_set.contains(&rust_field_name) {
                continue;
            }

            // Apply pick filter
            if !pick_set.is_empty() && !pick_set.contains(&rust_field_name) {
                continue;
            }

            // Check if this is a SeaORM relation type
            let is_relation = is_seaorm_relation_type(&field.ty);

            // Get field components, applying partial wrapping if needed
            let original_ty = &field.ty;
            let should_wrap_option = (partial_all || partial_set.contains(&rust_field_name))
                && !is_option_type(original_ty)
                && !is_relation; // Don't wrap relations in another Option

            // Determine field type: convert relation types to Schema types
            let (field_ty, relation_info): (Box<dyn quote::ToTokens>, Option<RelationFieldInfo>) =
                if is_relation {
                    // Convert HasOne/HasMany/BelongsTo to Schema type
                    if let Some((converted, mut rel_info)) =
                        convert_relation_type_to_schema_with_info(
                            original_ty,
                            &field.attrs,
                            &parsed_struct,
                            &source_module_path,
                            field.ident.clone().unwrap(),
                        )
                    {
                        // NEW RULE: HasMany (reverse references) are excluded by default
                        // They can only be included via explicit `pick`
                        if rel_info.relation_type == "HasMany" {
                            // HasMany is only included if explicitly picked
                            if !pick_set.contains(&rust_field_name) {
                                continue;
                            }
                            // When HasMany IS picked, generate inline type with ALL relations stripped
                            if let Some(inline_type) = generate_inline_relation_type_no_relations(
                                new_type_name,
                                &rel_info,
                                input.schema_name.as_deref(),
                            ) {
                                let inline_type_def = generate_inline_type_definition(&inline_type);
                                inline_type_definitions.push(inline_type_def);

                                let inline_type_name = &inline_type.type_name;
                                let included_fields: Vec<String> = inline_type
                                    .fields
                                    .iter()
                                    .map(|f| f.name.to_string())
                                    .collect();

                                rel_info.inline_type_info =
                                    Some((inline_type.type_name.clone(), included_fields));

                                let inline_field_ty = quote! { Vec<#inline_type_name> };
                                (Box::new(inline_field_ty), Some(rel_info))
                            } else {
                                continue;
                            }
                        } else {
                            // BelongsTo/HasOne: Include by default
                            // Check for circular references and potentially use inline type
                            if let Some(inline_type) = generate_inline_relation_type(
                                new_type_name,
                                &rel_info,
                                &source_module_path,
                                input.schema_name.as_deref(),
                            ) {
                                // Generate inline type definition
                                let inline_type_def = generate_inline_type_definition(&inline_type);
                                inline_type_definitions.push(inline_type_def);

                                // Use inline type instead of direct schema reference
                                let inline_type_name = &inline_type.type_name;
                                let circular_fields: Vec<String> = inline_type
                                    .fields
                                    .iter()
                                    .map(|f| f.name.to_string())
                                    .collect();

                                // Store inline type info
                                rel_info.inline_type_info =
                                    Some((inline_type.type_name.clone(), circular_fields));

                                // Generate field type using inline type
                                let inline_field_ty = if rel_info.is_optional {
                                    quote! { Option<Box<#inline_type_name>> }
                                } else {
                                    quote! { Box<#inline_type_name> }
                                };

                                (Box::new(inline_field_ty), Some(rel_info))
                            } else {
                                // No circular refs, use original schema path
                                (Box::new(converted), Some(rel_info))
                            }
                        }
                    } else {
                        // Fallback: skip if conversion fails
                        continue;
                    }
                } else {
                    // Convert SeaORM datetime types to chrono equivalents
                    // Also resolves local types to absolute paths
                    let converted_ty = convert_type_with_chrono(original_ty, &source_module_path);
                    if should_wrap_option {
                        (Box::new(quote! { Option<#converted_ty> }), None)
                    } else {
                        (Box::new(converted_ty), None)
                    }
                };

            // Collect relation info
            if let Some(info) = relation_info {
                relation_fields.push(info);
            }
            let vis = &field.vis;
            let source_field_ident = field.ident.clone().unwrap();

            // Filter field attributes: only keep serde attributes, remove sea_orm and others
            // This is important when using schema_type! with models from other files
            // that may have ORM-specific attributes we don't want in the generated struct
            let serde_field_attrs: Vec<_> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("serde"))
                .collect();

            // Check if field should be renamed
            if let Some(new_name) = rename_map.get(&rust_field_name) {
                // Create new identifier for the field
                let new_field_ident =
                    syn::Ident::new(new_name, field.ident.as_ref().unwrap().span());

                // Filter out serde(rename) attributes from the serde attrs
                let filtered_attrs: Vec<_> = serde_field_attrs
                    .iter()
                    .filter(|attr| {
                        // Check if it's a rename attribute
                        let mut has_rename = false;
                        let _ = attr.parse_nested_meta(|meta| {
                            if meta.path.is_ident("rename") {
                                has_rename = true;
                            }
                            Ok(())
                        });
                        !has_rename
                    })
                    .collect();

                // Determine the JSON name: use existing serde(rename) if present, otherwise rust field name
                let json_name =
                    extract_field_rename(&field.attrs).unwrap_or_else(|| rust_field_name.clone());

                field_tokens.push(quote! {
                    #(#filtered_attrs)*
                    #[serde(rename = #json_name)]
                    #vis #new_field_ident: #field_ty
                });

                // Track mapping: new field name <- source field name
                field_mappings.push((
                    new_field_ident,
                    source_field_ident,
                    should_wrap_option,
                    is_relation,
                ));
            } else {
                // No rename, keep field with only serde attrs
                let field_ident = field.ident.clone().unwrap();

                field_tokens.push(quote! {
                    #(#serde_field_attrs)*
                    #vis #field_ident: #field_ty
                });

                // Track mapping: same name
                field_mappings.push((
                    field_ident.clone(),
                    field_ident,
                    should_wrap_option,
                    is_relation,
                ));
            }
        }
    }

    // Add new fields from `add` parameter
    if let Some(ref add_fields) = input.add {
        for (field_name, field_ty) in add_fields {
            let field_ident = syn::Ident::new(field_name, proc_macro2::Span::call_site());
            field_tokens.push(quote! {
                pub #field_ident: #field_ty
            });
        }
    }

    // Build derive list
    let clone_derive = if input.derive_clone {
        quote! { Clone, }
    } else {
        quote! {}
    };

    // Conditionally include Schema derive based on ignore_schema flag
    // Also generate #[schema(name = "...")] attribute if custom name is provided AND Schema is derived
    let (schema_derive, schema_name_attr) = if input.ignore_schema {
        (quote! {}, quote! {})
    } else if let Some(ref name) = input.schema_name {
        (
            quote! { vespera::Schema },
            quote! { #[schema(name = #name)] },
        )
    } else {
        (quote! { vespera::Schema }, quote! {})
    };

    // Check if there are any relation fields
    let has_relation_fields = field_mappings.iter().any(|(_, _, _, is_rel)| *is_rel);

    // Generate From impl only if:
    // 1. `add` is not used (can't auto-populate added fields)
    // 2. There are no relation fields (relation fields don't exist on source Model)
    let source_type = &input.source_type;
    let from_impl = if input.add.is_none() && !has_relation_fields {
        let field_assignments: Vec<_> = field_mappings
            .iter()
            .map(|(new_ident, source_ident, wrapped, _is_relation)| {
                if *wrapped {
                    quote! { #new_ident: Some(source.#source_ident) }
                } else {
                    quote! { #new_ident: source.#source_ident }
                }
            })
            .collect();

        quote! {
            impl From<#source_type> for #new_type_name {
                fn from(source: #source_type) -> Self {
                    Self {
                        #(#field_assignments),*
                    }
                }
            }
        }
    } else {
        quote! {}
    };

    // Generate from_model impl for SeaORM Models WITH relations
    // - No relations: Use `From` trait (generated above)
    // - Has relations: async fn from_model(model: Model, db: &DatabaseConnection) -> Result<Self, DbErr>
    let from_model_impl = if is_source_seaorm_model && input.add.is_none() && has_relation_fields {
        generate_from_model_with_relations(
            new_type_name,
            source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            schema_storage,
        )
    } else {
        quote! {}
    };

    // Generate the new struct (with inline types for circular relations first)
    let generated_tokens = quote! {
        // Inline types for circular relation references
        #(#inline_type_definitions)*

        #[derive(serde::Serialize, serde::Deserialize, #clone_derive #schema_derive)]
        #schema_name_attr
        #[serde(rename_all = #effective_rename_all)]
        #(#serde_attrs_without_rename_all)*
        pub struct #new_type_name {
            #(#field_tokens),*
        }

        #from_impl
        #from_model_impl
    };

    // If custom name is provided, create metadata for direct registration
    // This ensures the schema appears in OpenAPI even when `ignore` is set
    let metadata = if let Some(ref custom_name) = input.schema_name {
        // Build struct definition string for metadata (without derives/attrs for parsing)
        let struct_def = quote! {
            #[serde(rename_all = #effective_rename_all)]
            #(#serde_attrs_without_rename_all)*
            pub struct #new_type_name {
                #(#field_tokens),*
            }
        };
        Some(StructMetadata::new(
            custom_name.clone(),
            struct_def.to_string(),
        ))
    } else {
        None
    };

    Ok((generated_tokens, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_schema_input_simple() {
        let tokens = quote::quote!(User);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        assert!(input.omit.is_none());
        assert!(input.pick.is_none());
    }

    #[test]
    fn test_parse_schema_input_with_omit() {
        let tokens = quote::quote!(User, omit = ["password", "secret"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let omit = input.omit.unwrap();
        assert_eq!(omit, vec!["password", "secret"]);
    }

    #[test]
    fn test_parse_schema_input_with_pick() {
        let tokens = quote::quote!(User, pick = ["id", "name"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let pick = input.pick.unwrap();
        assert_eq!(pick, vec!["id", "name"]);
    }

    #[test]
    fn test_parse_schema_input_omit_and_pick_error() {
        let tokens = quote::quote!(User, omit = ["a"], pick = ["b"]);
        let result: syn::Result<SchemaInput> = syn::parse2(tokens);
        assert!(result.is_err());
    }

    // schema_type! tests

    #[test]
    fn test_parse_schema_type_input_simple() {
        let tokens = quote::quote!(CreateUser from User);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "CreateUser");
        assert!(input.omit.is_none());
        assert!(input.pick.is_none());
        assert!(input.rename.is_none());
        assert!(input.derive_clone);
    }

    #[test]
    fn test_parse_schema_type_input_with_pick() {
        let tokens = quote::quote!(CreateUser from User, pick = ["name", "email"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "CreateUser");
        let pick = input.pick.unwrap();
        assert_eq!(pick, vec!["name", "email"]);
    }

    #[test]
    fn test_parse_schema_type_input_with_rename() {
        let tokens =
            quote::quote!(UserDTO from User, rename = [("id", "user_id"), ("name", "full_name")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "UserDTO");
        let rename = input.rename.unwrap();
        assert_eq!(rename.len(), 2);
        assert_eq!(rename[0], ("id".to_string(), "user_id".to_string()));
        assert_eq!(rename[1], ("name".to_string(), "full_name".to_string()));
    }

    #[test]
    fn test_parse_schema_type_input_with_single_rename() {
        let tokens = quote::quote!(UserDTO from User, rename = [("id", "user_id")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let rename = input.rename.unwrap();
        assert_eq!(rename.len(), 1);
        assert_eq!(rename[0], ("id".to_string(), "user_id".to_string()));
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_rename() {
        let tokens =
            quote::quote!(UserDTO from User, pick = ["id", "name"], rename = [("id", "user_id")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
        assert_eq!(
            input.rename.unwrap(),
            vec![("id".to_string(), "user_id".to_string())]
        );
    }

    #[test]
    fn test_parse_schema_type_input_with_omit_and_rename() {
        let tokens =
            quote::quote!(UserPublic from User, omit = ["password"], rename = [("id", "user_id")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.omit.unwrap(), vec!["password"]);
        assert_eq!(
            input.rename.unwrap(),
            vec![("id".to_string(), "user_id".to_string())]
        );
    }

    #[test]
    fn test_parse_schema_type_input_with_clone_false() {
        let tokens = quote::quote!(NonCloneUser from User, clone = false);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert!(!input.derive_clone);
    }

    #[test]
    fn test_parse_schema_type_input_unknown_param_error() {
        let tokens = quote::quote!(UserDTO from User, unknown = ["a"]);
        let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
        assert!(result.is_err());
        // Note: Can't use unwrap_err() because SchemaTypeInput doesn't impl Debug (contains syn::Type)
        match result {
            Err(e) => assert!(e.to_string().contains("unknown parameter")),
            Ok(_) => panic!("Expected error"),
        }
    }

    // Tests for `add` parameter

    #[test]
    fn test_parse_schema_type_input_with_add_single() {
        let tokens = quote::quote!(UserWithTimestamp from User, add = [("created_at": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "UserWithTimestamp");
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "created_at");
    }

    #[test]
    fn test_parse_schema_type_input_with_add_multiple() {
        let tokens = quote::quote!(UserWithMeta from User, add = [("created_at": String), ("updated_at": Option<String>)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let add = input.add.unwrap();
        assert_eq!(add.len(), 2);
        assert_eq!(add[0].0, "created_at");
        assert_eq!(add[1].0, "updated_at");
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_add() {
        let tokens = quote::quote!(CreateUserWithMeta from User, pick = ["name", "email"], add = [("request_id": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "request_id");
    }

    #[test]
    fn test_parse_schema_type_input_with_omit_and_add() {
        let tokens = quote::quote!(UserPublicWithMeta from User, omit = ["password"], add = [("display_name": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.omit.unwrap(), vec!["password"]);
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "display_name");
    }

    #[test]
    fn test_parse_schema_type_input_with_add_complex_type() {
        let tokens = quote::quote!(UserWithVec from User, add = [("tags": Vec<String>)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "tags");
    }

    // Tests for `partial` parameter

    #[test]
    fn test_parse_schema_type_input_with_partial_all() {
        let tokens = quote::quote!(UpdateUser from User, partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert!(matches!(input.partial, Some(PartialMode::All)));
    }

    #[test]
    fn test_parse_schema_type_input_with_partial_fields() {
        let tokens = quote::quote!(UpdateUser from User, partial = ["name", "email"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        match input.partial {
            Some(PartialMode::Fields(fields)) => {
                assert_eq!(fields, vec!["name", "email"]);
            }
            _ => panic!("Expected PartialMode::Fields"),
        }
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_partial() {
        let tokens = quote::quote!(UpdateUser from User, pick = ["name", "email"], partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
        assert!(matches!(input.partial, Some(PartialMode::All)));
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_partial_fields() {
        let tokens =
            quote::quote!(UpdateUser from User, pick = ["name", "email"], partial = ["name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
        match input.partial {
            Some(PartialMode::Fields(fields)) => {
                assert_eq!(fields, vec!["name"]);
            }
            _ => panic!("Expected PartialMode::Fields"),
        }
    }

    #[test]
    fn test_generate_schema_type_code_with_partial_all() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub bio: Option<String> }",
        )];

        let tokens = quote::quote!(UpdateUser from User, partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // id and name should be wrapped in Option, bio already Option stays unchanged
        assert!(output.contains("Option < i32 >"));
        assert!(output.contains("Option < String >"));
    }

    #[test]
    fn test_generate_schema_type_code_with_partial_fields() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub email: String }",
        )];

        let tokens = quote::quote!(UpdateUser from User, partial = ["name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // name should be Option<String>, but id and email should remain unwrapped
        assert!(output.contains("UpdateUser"));
    }

    #[test]
    fn test_generate_schema_type_code_partial_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(UpdateUser from User, partial = ["nonexistent"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_partial_from_impl_wraps_some() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(UpdateUser from User, partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // From impl should wrap values in Some()
        assert!(output.contains("Some (source . id)"));
        assert!(output.contains("Some (source . name)"));
    }

    // =========================================================================
    // Tests for generate_schema_code() - success paths
    // =========================================================================

    fn create_test_struct_metadata(
        name: &str,
        definition: &str,
    ) -> crate::metadata::StructMetadata {
        crate::metadata::StructMetadata::new(name.to_string(), definition.to_string())
    }

    #[test]
    fn test_generate_schema_code_simple_struct() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(User);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
        assert!(output.contains("Schema"));
    }

    #[test]
    fn test_generate_schema_code_with_omit() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )];

        let tokens = quote::quote!(User, omit = ["password"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        // Should have id and name but not password in properties
        assert!(output.contains("properties"));
    }

    #[test]
    fn test_generate_schema_code_with_pick() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub email: String }",
        )];

        let tokens = quote::quote!(User, pick = ["id", "name"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
    }

    // =========================================================================
    // Tests for generate_schema_code() - error paths
    // =========================================================================

    #[test]
    fn test_generate_schema_code_type_not_found() {
        let storage: Vec<crate::metadata::StructMetadata> = vec![];

        let tokens = quote::quote!(NonExistent);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_code_malformed_definition() {
        let storage = vec![create_test_struct_metadata(
            "BadStruct",
            "this is not valid rust code {{{",
        )];

        let tokens = quote::quote!(BadStruct);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to parse"));
    }

    // =========================================================================
    // Tests for schema_ref_to_tokens()
    // =========================================================================

    #[test]
    fn test_schema_ref_to_tokens_ref_variant() {
        use vespera_core::schema::{Reference, SchemaRef};

        let schema_ref = SchemaRef::Ref(Reference::new("#/components/schemas/User".to_string()));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();

        assert!(output.contains("SchemaRef :: Ref"));
        assert!(output.contains("Reference :: new"));
    }

    #[test]
    fn test_schema_ref_to_tokens_inline_variant() {
        use vespera_core::schema::{Schema, SchemaRef, SchemaType};

        let schema = Schema::new(SchemaType::String);
        let schema_ref = SchemaRef::Inline(Box::new(schema));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();

        assert!(output.contains("SchemaRef :: Inline"));
        assert!(output.contains("Box :: new"));
    }

    // =========================================================================
    // Tests for schema_to_tokens()
    // =========================================================================

    #[test]
    fn test_schema_to_tokens_string_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::String);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: String"));
    }

    #[test]
    fn test_schema_to_tokens_integer_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Integer);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Integer"));
    }

    #[test]
    fn test_schema_to_tokens_number_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Number);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Number"));
    }

    #[test]
    fn test_schema_to_tokens_boolean_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Boolean);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Boolean"));
    }

    #[test]
    fn test_schema_to_tokens_array_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Array);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Array"));
    }

    #[test]
    fn test_schema_to_tokens_object_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Object);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Object"));
    }

    #[test]
    fn test_schema_to_tokens_null_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Null);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Null"));
    }

    #[test]
    fn test_schema_to_tokens_with_format() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::String);
        schema.format = Some("date-time".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("date-time"));
    }

    #[test]
    fn test_schema_to_tokens_with_nullable() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::String);
        schema.nullable = Some(true);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("Some (true)"));
    }

    #[test]
    fn test_schema_to_tokens_with_ref_path() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::Object);
        schema.ref_path = Some("#/components/schemas/User".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("#/components/schemas/User"));
    }

    #[test]
    fn test_schema_to_tokens_with_items() {
        use vespera_core::schema::{Schema, SchemaRef, SchemaType};

        let mut schema = Schema::new(SchemaType::Array);
        let item_schema = Schema::new(SchemaType::String);
        schema.items = Some(Box::new(SchemaRef::Inline(Box::new(item_schema))));
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("items"));
        assert!(output.contains("Some (Box :: new"));
    }

    #[test]
    fn test_schema_to_tokens_with_properties() {
        use std::collections::BTreeMap;
        use vespera_core::schema::{Schema, SchemaRef, SchemaType};

        let mut schema = Schema::new(SchemaType::Object);
        let mut props = BTreeMap::new();
        props.insert(
            "name".to_string(),
            SchemaRef::Inline(Box::new(Schema::new(SchemaType::String))),
        );
        schema.properties = Some(props);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("properties"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_schema_to_tokens_with_required() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::Object);
        schema.required = Some(vec!["id".to_string(), "name".to_string()]);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("required"));
        assert!(output.contains("id"));
        assert!(output.contains("name"));
    }

    // =========================================================================
    // Tests for generate_schema_type_code() - validation errors
    // =========================================================================

    #[test]
    fn test_generate_schema_type_code_pick_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(NewUser from User, pick = ["nonexistent"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_omit_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(NewUser from User, omit = ["nonexistent"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_rename_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(NewUser from User, rename = [("nonexistent", "new_name")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_type_not_found() {
        let storage: Vec<crate::metadata::StructMetadata> = vec![];

        let tokens = quote::quote!(NewUser from NonExistent);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_type_code_success() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(CreateUser from User, pick = ["name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("CreateUser"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_generate_schema_type_code_with_omit() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )];

        let tokens = quote::quote!(SafeUser from User, omit = ["password"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("SafeUser"));
        // Should not contain password
        assert!(!output.contains("password"));
    }

    #[test]
    fn test_generate_schema_type_code_with_add() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("UserWithExtra"));
        assert!(output.contains("extra"));
    }

    #[test]
    fn test_generate_schema_type_code_generates_from_impl() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        // Without add parameter, should generate From impl
        let tokens = quote::quote!(UserResponse from User, pick = ["id", "name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("impl From"));
        assert!(output.contains("for UserResponse"));
    }

    #[test]
    fn test_generate_schema_type_code_no_from_impl_with_add() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        // With add parameter, should NOT generate From impl
        let tokens = quote::quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should NOT contain From impl when add is used
        assert!(!output.contains("impl From"));
    }

    // =========================================================================
    // Tests for is_option_type()
    // =========================================================================

    #[test]
    fn test_is_option_type_true() {
        let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
        assert!(is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_false() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(!is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_vec_false() {
        let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
        assert!(!is_option_type(&ty));
    }

    // =========================================================================
    // Tests for extract_type_name()
    // =========================================================================

    #[test]
    fn test_extract_type_name_simple() {
        let ty: syn::Type = syn::parse_str("User").unwrap();
        let name = extract_type_name(&ty).unwrap();
        assert_eq!(name, "User");
    }

    #[test]
    fn test_extract_type_name_with_path() {
        let ty: syn::Type = syn::parse_str("crate::models::User").unwrap();
        let name = extract_type_name(&ty).unwrap();
        assert_eq!(name, "User");
    }

    #[test]
    fn test_extract_type_name_non_path_error() {
        // Reference type is not a Type::Path
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let result = extract_type_name(&ty);
        assert!(result.is_err());
    }

    // =========================================================================
    // Tests for rename_all parsing
    // =========================================================================

    #[test]
    fn test_parse_schema_type_input_with_rename_all() {
        let tokens = quote::quote!(NewType from User, rename_all = "snake_case");
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
    }

    #[test]
    fn test_parse_schema_type_input_rename_all_with_other_params() {
        // rename_all should work alongside other parameters
        let tokens =
            quote::quote!(NewType from User, pick = ["id", "name"], rename_all = "snake_case");
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
        assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
    }
}
