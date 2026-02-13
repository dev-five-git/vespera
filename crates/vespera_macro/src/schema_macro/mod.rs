//! Schema macro implementation
//!
//! Provides macros for generating OpenAPI schemas from struct types:
//! - `schema!` - Generate Schema value with optional field filtering
//! - `schema_type!` - Generate new struct type derived from existing type

mod circular;
mod codegen;
mod file_lookup;
mod from_model;
mod inline_types;
mod input;
mod seaorm;
mod transformation;
pub mod type_utils;
mod validation;

use std::collections::HashSet;

use codegen::generate_filtered_schema;
use file_lookup::find_struct_from_path;
use from_model::generate_from_model_with_relations;
use inline_types::{
    generate_inline_relation_type, generate_inline_relation_type_no_relations,
    generate_inline_type_definition,
};
pub use input::{PartialMode, SchemaInput, SchemaTypeInput};
use proc_macro2::TokenStream;
use quote::quote;
use seaorm::{
    RelationFieldInfo, convert_relation_type_to_schema_with_info, convert_type_with_chrono,
};
use transformation::{
    build_omit_set, build_partial_config, build_pick_set, build_rename_map, determine_rename_all,
    extract_doc_attrs, extract_field_serde_attrs, extract_form_data_attrs,
    extract_serde_attrs_without_rename_all, filter_out_serde_rename, should_skip_field,
    should_wrap_in_option,
};
use type_utils::{
    extract_module_path, extract_type_name, is_option_type, is_qualified_path, is_seaorm_model,
    is_seaorm_relation_type,
};
use validation::{
    extract_source_field_names, validate_omit_fields, validate_partial_fields,
    validate_pick_fields, validate_rename_fields,
};

use crate::{
    metadata::StructMetadata,
    parser::{extract_field_rename, strip_raw_prefix},
};

/// Generate schema code from a struct with optional field filtering
pub fn generate_schema_code(
    input: &SchemaInput,
    schema_storage: &[StructMetadata],
) -> Result<TokenStream, syn::Error> {
    // Extract type name from the Type
    let type_name = extract_type_name(&input.ty)?;

    // Find struct definition in storage
    let struct_def = schema_storage.iter().find(|s| s.name == type_name).ok_or_else(|| syn::Error::new_spanned(&input.ty, format!("type `{}` not found. Make sure it has #[derive(Schema)] before this macro invocation", type_name)))?;

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
            // Always use the module path from file lookup for qualified paths
            // The file lookup derives module path from actual file location, which is more accurate
            // for resolving relative paths like `super::user::Entity`
            source_module_path = module_path;
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
    let source_field_names = extract_source_field_names(&parsed_struct);

    // Validate all field references exist in source struct
    validate_pick_fields(
        input.pick.as_ref(),
        &source_field_names,
        &input.source_type,
        &source_type_name,
    )?;
    validate_omit_fields(
        input.omit.as_ref(),
        &source_field_names,
        &input.source_type,
        &source_type_name,
    )?;
    validate_rename_fields(
        input.rename.as_ref(),
        &source_field_names,
        &input.source_type,
        &source_type_name,
    )?;
    let partial_fields_to_validate = match &input.partial {
        Some(PartialMode::Fields(fields)) => Some(fields),
        _ => None,
    };
    validate_partial_fields(
        partial_fields_to_validate,
        &source_field_names,
        &input.source_type,
        &source_type_name,
    )?;

    // Build filter sets and rename map
    let omit_set = build_omit_set(input.omit.clone());
    let pick_set = build_pick_set(input.pick.clone());
    let (partial_all, partial_set) = build_partial_config(&input.partial);
    let rename_map = build_rename_map(input.rename.clone());

    // Extract serde attributes from source struct, excluding rename_all (we'll handle it separately)
    let serde_attrs_without_rename_all =
        extract_serde_attrs_without_rename_all(&parsed_struct.attrs);

    // Extract doc comments from source struct to carry over to generated struct
    let struct_doc_attrs = extract_doc_attrs(&parsed_struct.attrs);

    // Determine the effective rename_all strategy
    let effective_rename_all =
        determine_rename_all(input.rename_all.as_ref(), &parsed_struct.attrs);

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

            // Apply omit/pick filters
            if should_skip_field(&rust_field_name, &omit_set, &pick_set) {
                continue;
            }

            // Check if this is a SeaORM relation type
            let is_relation = is_seaorm_relation_type(&field.ty);

            // In multipart mode, skip ALL relation fields (multipart forms can't represent nested objects)
            if input.multipart && is_relation {
                continue;
            }

            // Get field components, applying partial wrapping if needed
            let original_ty = &field.ty;
            let should_wrap_option = should_wrap_in_option(
                &rust_field_name,
                partial_all,
                &partial_set,
                is_option_type(original_ty),
                is_relation,
            );

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
                                &source_module_path,
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

            // Extract doc attributes to carry over comments to the generated struct
            let doc_attrs = extract_doc_attrs(&field.attrs);

            if input.multipart {
                // Multipart mode: emit form_data attrs, suppress serde attrs
                let form_data_attrs = extract_form_data_attrs(&field.attrs);

                // Check if field should be renamed (rename still applies to Rust field names)
                if let Some(new_name) = rename_map.get(&rust_field_name) {
                    let new_field_ident =
                        syn::Ident::new(new_name, field.ident.as_ref().unwrap().span());

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
                        #(#form_data_attrs)*
                        #vis #new_field_ident: #field_ty
                    });

                    field_mappings.push((
                        new_field_ident,
                        source_field_ident,
                        should_wrap_option,
                        is_relation,
                    ));
                } else {
                    let field_ident = field.ident.clone().unwrap();

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
                        #(#form_data_attrs)*
                        #vis #field_ident: #field_ty
                    });

                    field_mappings.push((
                        field_ident.clone(),
                        field_ident,
                        should_wrap_option,
                        is_relation,
                    ));
                }
            } else {
                // Normal (serde) mode: emit serde attrs
                // Filter field attributes: keep serde and doc attributes, remove sea_orm and others
                // This is important when using schema_type! with models from other files
                // that may have ORM-specific attributes we don't want in the generated struct
                let serde_field_attrs = extract_field_serde_attrs(&field.attrs);

                // Check if field should be renamed
                if let Some(new_name) = rename_map.get(&rust_field_name) {
                    // Create new identifier for the field
                    let new_field_ident =
                        syn::Ident::new(new_name, field.ident.as_ref().unwrap().span());

                    // Filter out serde(rename) attributes from the serde attrs
                    let filtered_attrs = filter_out_serde_rename(&serde_field_attrs);

                    // Determine the JSON name: use existing serde(rename) if present, otherwise rust field name
                    let json_name = extract_field_rename(&field.attrs)
                        .unwrap_or_else(|| rust_field_name.clone());

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
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
                    // No rename, keep field with serde and doc attrs
                    let field_ident = field.ident.clone().unwrap();

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
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
    // In multipart mode, force clone = false (FieldData<NamedTempFile> doesn't implement Clone)
    let derive_clone = if input.multipart {
        false
    } else {
        input.derive_clone
    };
    let clone_derive = if derive_clone {
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

    // In multipart mode, skip From and from_model impls entirely
    let source_type = &input.source_type;
    let (from_impl, from_model_impl) = if input.multipart {
        (quote! {}, quote! {})
    } else {
        // Generate From impl only if:
        // 1. `add` is not used (can't auto-populate added fields)
        // 2. There are no relation fields (relation fields don't exist on source Model)
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
        let from_model_impl =
            if is_source_seaorm_model && input.add.is_none() && has_relation_fields {
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

        (from_impl, from_model_impl)
    };

    // Generate the new struct (with inline types for circular relations first)
    let generated_tokens = if input.multipart {
        // Multipart mode: derive TryFromMultipart instead of serde
        // Still emit #[serde(rename_all = ...)] so Schema derive can read it for OpenAPI field naming
        // (Schema derive registers `serde` as a helper attribute, so this is valid without Serialize/Deserialize)
        quote! {
            #(#inline_type_definitions)*

            #(#struct_doc_attrs)*
            #[derive(vespera::axum_typed_multipart::TryFromMultipart, #clone_derive #schema_derive)]
            #schema_name_attr
            #[serde(rename_all = #effective_rename_all)]
            pub struct #new_type_name {
                #(#field_tokens),*
            }
        }
    } else {
        // Normal serde mode
        quote! {
            // Inline types for circular relation references
            #(#inline_type_definitions)*

            #(#struct_doc_attrs)*
            #[derive(serde::Serialize, serde::Deserialize, #clone_derive #schema_derive)]
            #schema_name_attr
            #[serde(rename_all = #effective_rename_all)]
            #(#serde_attrs_without_rename_all)*
            pub struct #new_type_name {
                #(#field_tokens),*
            }

            #from_impl
            #from_model_impl
        }
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
mod tests;
