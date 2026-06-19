//! Inline type generation for circular references
//!
//! When schemas have circular references, we generate inline types that
//! exclude the circular fields to prevent infinite recursion.

use proc_macro2::TokenStream;
use quote::quote;

use super::{
    file_cache::{get_circular_analysis, get_module_path_from_schema_path},
    file_lookup::find_model_from_schema_path,
    seaorm::{RelationFieldInfo, convert_type_with_chrono},
    type_utils::{is_seaorm_relation_type, snake_to_pascal_case},
};
use crate::parser::{extract_rename_all, extract_skip};

/// Information about an inline relation type to generate
pub struct InlineRelationType {
    /// Name of the inline type (e.g., `MemoResponseRel_User`)
    pub type_name: syn::Ident,
    /// Fields to include (excluding circular references)
    pub fields: Vec<InlineField>,
    /// The effective `rename_all` strategy
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
/// The `schema_name_override` parameter allows using a custom schema name (e.g., "`MemoSchema`")
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

    generate_inline_relation_type_from_def(
        parent_type_name,
        rel_info,
        source_module_path,
        schema_name_override,
        model_def,
    )
}

/// Internal version that accepts model definition directly (for testing)
pub fn generate_inline_relation_type_from_def(
    parent_type_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    source_module_path: &[String],
    schema_name_override: Option<&str>,
    model_def: &str,
) -> Option<InlineRelationType> {
    generate_inline_type_core(
        parent_type_name,
        rel_info,
        source_module_path,
        schema_name_override,
        model_def,
        true, // check circular fields
    )
}

/// Generate inline relation type for `HasMany` with ALL relations stripped.
///
/// When a `HasMany` relation is explicitly picked, the nested items should have
/// NO relation fields at all (not even FK relations). This prevents infinite
/// nesting and keeps the schema simple.
///
/// Example: If `UserSchema` picks "memos", each memo in the list will have
/// id, `user_id`, title, content, etc. but NO user or comments relations.
pub fn generate_inline_relation_type_no_relations(
    parent_type_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    source_module_path: &[String],
    schema_name_override: Option<&str>,
) -> Option<InlineRelationType> {
    // Find the target model definition
    let schema_path_str = rel_info.schema_path.to_string();
    let model_metadata = find_model_from_schema_path(&schema_path_str)?;
    let model_def = &model_metadata.definition;

    generate_inline_relation_type_no_relations_from_def(
        parent_type_name,
        rel_info,
        source_module_path,
        schema_name_override,
        model_def,
    )
}

/// Internal version that accepts model definition directly (for testing)
pub fn generate_inline_relation_type_no_relations_from_def(
    parent_type_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    source_module_path: &[String],
    schema_name_override: Option<&str>,
    model_def: &str,
) -> Option<InlineRelationType> {
    generate_inline_type_core(
        parent_type_name,
        rel_info,
        source_module_path,
        schema_name_override,
        model_def,
        false, // skip all relations without circular check
    )
}

/// Core implementation shared by both circular-reference and no-relations variants.
///
/// When `check_circular` is `true`:
///   - Detects circular fields via `get_circular_analysis`
///   - Returns `None` if no circular fields exist (no inline type needed)
///   - Excludes circular fields from the generated type
///
/// When `check_circular` is `false`:
///   - Skips ALL relation types unconditionally
///   - Always proceeds (no early return)
fn generate_inline_type_core(
    parent_type_name: &syn::Ident,
    rel_info: &RelationFieldInfo,
    source_module_path: &[String],
    schema_name_override: Option<&str>,
    model_def: &str,
    check_circular: bool,
) -> Option<InlineRelationType> {
    // Parse the model struct
    let parsed_model: syn::ItemStruct = super::file_cache::parse_struct_cached(model_def).ok()?;

    // IMPORTANT: Use the TARGET model's module path for type resolution, not the parent's.
    // This ensures enum types are resolved to the correct module path
    // instead of incorrectly using the parent module path.
    let target_module_path = get_module_path_from_schema_path(&rel_info.schema_path);
    let effective_module_path = if target_module_path.is_empty() {
        source_module_path
    } else {
        &target_module_path
    };

    // Detect circular fields only when requested
    let circular_fields: Vec<String> = if check_circular {
        let fields = get_circular_analysis(source_module_path, model_def).circular_fields;
        // If no circular fields, no need for inline type
        if fields.is_empty() {
            return None;
        }
        fields
    } else {
        Vec::new()
    };

    // Get rename_all from model (or default to camelCase)
    let rename_all =
        extract_rename_all(&parsed_model.attrs).unwrap_or_else(|| "camelCase".to_string());

    // Generate inline type name: {SchemaName}_{Field}
    // Use custom schema name if provided, otherwise use the Rust struct name
    let parent_name = schema_name_override.map_or_else(
        || parent_type_name.to_string(),
        std::string::ToString::to_string,
    );
    let field_name_pascal = snake_to_pascal_case(&rel_info.field_name.to_string());
    let inline_type_name = syn::Ident::new(
        &format!("{parent_name}_{field_name_pascal}"),
        proc_macro2::Span::call_site(),
    );

    // Collect fields, excluding circular ones and/or relation types
    let mut fields = Vec::with_capacity(8);
    if let syn::Fields::Named(fields_named) = &parsed_model.fields {
        for field in &fields_named.named {
            let field_ident = field.ident.as_ref()?;

            // Skip circular fields (only when check_circular is true)
            if check_circular {
                let field_name_str = field_ident.to_string();
                if circular_fields.contains(&field_name_str) {
                    continue;
                }
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

            // Convert SeaORM datetime types to chrono equivalents
            // Use the target model's module path to correctly resolve enum types
            let converted_ty = convert_type_with_chrono(&field.ty, effective_module_path);
            fields.push(InlineField {
                name: field_ident.clone(),
                ty: converted_ty,
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

/// Generate the struct definition `TokenStream` for an inline relation type
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

#[cfg(test)]
mod tests;
