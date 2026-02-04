//! Inline type generation for circular references
//!
//! When schemas reference each other (e.g., User ↔ Memo), we generate inline types
//! that exclude circular fields to prevent infinite recursion.

use proc_macro2::TokenStream;
use quote::quote;

use super::file_lookup::find_model_from_schema_path;
use super::relation::{RelationFieldInfo, detect_circular_fields};
use super::type_utils::{capitalize_first, is_seaorm_relation_type};
use crate::parser::{extract_rename_all, extract_skip};

/// Information about an inline relation type to generate
pub struct InlineRelationType {
    /// Name of the inline type (e.g., MemoResponseRel_User)
    pub type_name: syn::Ident,
    /// Fields to include (excluding circular references)
    pub fields: Vec<InlineField>,
    /// The effective rename_all strategy
    pub rename_all: String,
}

/// A field in an inline relation type
pub struct InlineField {
    pub name: syn::Ident,
    pub ty: TokenStream,
    pub attrs: Vec<syn::Attribute>,
}

/// Generate inline relation type definition for circular references.
///
/// When `MemoSchema.user` would reference `UserSchema` which has `memos: Vec<MemoSchema>`,
/// we instead generate an inline type `MemoSchema_User` that excludes the `memos` field.
///
/// The `schema_name_override` parameter allows using a custom schema name (e.g., "MemoSchema")
/// instead of the Rust struct name (e.g., "Schema") for the inline type name.
pub fn generate_inline_relation_type(
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

            // Keep serde and doc attributes
            let kept_attrs: Vec<syn::Attribute> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("serde") || attr.path().is_ident("doc"))
                .cloned()
                .collect();

            let field_ty = &field.ty;
            fields.push(InlineField {
                name: field_ident.clone(),
                ty: quote!(#field_ty),
                attrs: kept_attrs,
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
pub fn generate_inline_relation_type_no_relations(
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

            // Keep serde and doc attributes
            let kept_attrs: Vec<syn::Attribute> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("serde") || attr.path().is_ident("doc"))
                .cloned()
                .collect();

            let field_ty = &field.ty;
            fields.push(InlineField {
                name: field_ident.clone(),
                ty: quote!(#field_ty),
                attrs: kept_attrs,
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
pub fn generate_inline_type_definition(inline_type: &InlineRelationType) -> TokenStream {
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
