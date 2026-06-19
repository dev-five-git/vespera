//! `schema_type!` code generation.
//!
//! Hosts `generate_schema_type_code` - the orchestrator that turns a
//! `SchemaTypeInput` (parsed `schema_type!` invocation) into the generated
//! struct, `From`/`from_model` impls, inline circular types, and metadata.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;

use super::defaults::generate_sea_orm_default_attrs;
use super::file_cache;
use super::file_lookup::find_struct_from_path_detailed;
use super::from_model::generate_from_model_with_relations;
use super::inline_types::{
    generate_inline_relation_type, generate_inline_relation_type_no_relations,
    generate_inline_type_definition,
};
use super::input::{PartialMode, SchemaTypeInput};
use super::same_file_override::maybe_generate_same_file_relation_override;
use super::seaorm::{
    RelationFieldInfo, convert_relation_type_to_schema_with_info, convert_type_with_chrono,
    extract_sea_orm_default_value, has_sea_orm_primary_key,
};
use super::transformation::{
    build_omit_set, build_partial_config, build_pick_set, build_rename_map, determine_rename_all,
    extract_doc_attrs, extract_field_serde_attrs, extract_form_data_attrs,
    extract_serde_attrs_without_rename_all, filter_out_serde_rename, should_skip_field,
    should_wrap_in_option,
};
use super::type_utils::{
    extract_module_path, extract_type_name, is_option_type, is_qualified_path, is_seaorm_model,
    is_seaorm_relation_type,
};
use super::validation::{
    extract_source_field_names, validate_omit_fields, validate_partial_fields,
    validate_pick_fields, validate_rename_fields,
};
use crate::metadata::StructMetadata;
use crate::parser::{extract_field_rename, strip_raw_prefix_owned};

/// Generate a new struct type from an existing type with field filtering
///
/// Returns (`TokenStream`, Option<StructMetadata>) where the metadata is returned
/// when a custom `name` is provided (for direct registration in `SCHEMA_STORAGE`).
#[allow(clippy::too_many_lines)]
pub fn generate_schema_type_code(
    input: &SchemaTypeInput,
    schema_storage: &HashMap<String, StructMetadata>,
) -> Result<(TokenStream, Option<StructMetadata>), syn::Error> {
    // Extract type name from the source Type
    let source_type_name = extract_type_name(&input.source_type)?;

    // Extract the module path for resolving relative paths in relation types
    // This may be empty for simple names like `Model` - will be overridden below if found from file
    let mut source_module_path = extract_module_path(&input.source_type);

    // Find struct definition - check SCHEMA_STORAGE first (no file I/O),
    // fall back to file lookup for types not registered (e.g., SeaORM Model).
    let struct_def_owned: StructMetadata;
    let schema_name_hint = input.schema_name.as_deref();
    let struct_def = if is_qualified_path(&input.source_type) {
        // Qualified path: try storage first (avoids parse_file for Schema-derived types),
        // then file lookup for non-Schema types (e.g., SeaORM Model)
        if let Some(found) = schema_storage.get(&source_type_name) {
            found
        } else if let Ok((found, module_path)) =
            find_struct_from_path_detailed(&input.source_type, schema_name_hint)
        {
            struct_def_owned = found;
            // Use the module path from file lookup for qualified paths
            // The file lookup derives module path from actual file location, which is more accurate
            // for resolving relative paths like `super::user::Entity`
            source_module_path = module_path;
            &struct_def_owned
        } else {
            match find_struct_from_path_detailed(&input.source_type, schema_name_hint) {
                Ok((found, module_path)) => {
                    struct_def_owned = found;
                    source_module_path = module_path;
                    &struct_def_owned
                }
                Err(err) => return Err(err.to_syn_error(&input.source_type)),
            }
        }
    } else {
        // Simple name: try storage first (for same-file structs), then file lookup with schema name hint
        if let Some(found) = schema_storage.get(&source_type_name) {
            found
        } else if let Ok((found, module_path)) =
            find_struct_from_path_detailed(&input.source_type, schema_name_hint)
        {
            struct_def_owned = found;
            // For simple names, we MUST use the inferred module path from the file location
            // This is crucial for resolving relative paths like `super::user::Entity`
            source_module_path = module_path;
            &struct_def_owned
        } else {
            match find_struct_from_path_detailed(&input.source_type, schema_name_hint) {
                Ok((found, module_path)) => {
                    struct_def_owned = found;
                    source_module_path = module_path;
                    &struct_def_owned
                }
                Err(err) => return Err(err.to_syn_error(&input.source_type)),
            }
        }
    };

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = file_cache::parse_struct_cached(&struct_def.definition)
        .map_err(|e| {
            syn::Error::new_spanned(
                &input.source_type,
                format!("failed to parse struct definition for `{source_type_name}`: {e}"),
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
    let omit_set = build_omit_set(input.omit.as_ref());
    let pick_set = build_pick_set(input.pick.as_ref());
    let (partial_all, partial_set) = build_partial_config(&input.partial);
    let rename_map = build_rename_map(input.rename.as_ref());

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
    // Track default value functions generated from sea_orm(default_value)
    let mut default_functions: Vec<TokenStream> = Vec::new();
    // Track same-file relation override helpers
    let mut relation_override_helpers: Vec<TokenStream> = Vec::new();

    if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
        for field in &fields_named.named {
            let rust_field_name = field.ident.as_ref().map_or_else(
                || "unknown".to_string(),
                |i| strip_raw_prefix_owned(i.to_string()),
            );

            // Apply omit/pick filters
            if should_skip_field(&rust_field_name, &omit_set, &pick_set) {
                continue;
            }

            // Apply omit_default: skip fields with sea_orm(default_value) or sea_orm(primary_key)
            if input.omit_default
                && (extract_sea_orm_default_value(&field.attrs).is_some()
                    || has_sea_orm_primary_key(&field.attrs))
            {
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
                            if input.add.is_some()
                                && let Some((override_field_ty, helper_tokens)) =
                                    maybe_generate_same_file_relation_override(
                                        new_type_name,
                                        &rust_field_name,
                                        &rel_info,
                                        schema_storage,
                                    )?
                            {
                                relation_override_helpers.push(helper_tokens);
                                (Box::new(override_field_ty), Some(rel_info))
                            } else
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

            // Collect relation info — `.extend(...)` keeps the push site
            // out of an explicit closure so the coverage tracker
            // attributes the call to this source line.
            relation_fields.extend(relation_info);
            let vis: &syn::Visibility = &field.vis;
            let source_field_ident: syn::Ident = field.ident.clone().unwrap();

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

                // Generate serde default + schema(default) from sea_orm(default_value) or primary_key
                // Handles literal defaults, SQL function defaults, and implicit auto-increment
                let (serde_default_attr, schema_default_attr): (
                    proc_macro2::TokenStream,
                    proc_macro2::TokenStream,
                ) = generate_sea_orm_default_attrs(
                    &field.attrs,
                    new_type_name,
                    &rust_field_name,
                    original_ty,
                    &field_ty,
                    should_wrap_option || is_option_type(original_ty),
                    &mut default_functions,
                );

                // Check if field should be renamed
                if let Some(new_name) = rename_map.get(&rust_field_name) {
                    // Create new identifier for the field
                    let new_field_ident: syn::Ident =
                        syn::Ident::new(new_name, field.ident.as_ref().unwrap().span());

                    // Filter out serde(rename) attributes from the serde attrs
                    let filtered_attrs = filter_out_serde_rename(&serde_field_attrs);

                    // Determine the JSON name: use existing serde(rename) if present, otherwise rust field name
                    let json_name = extract_field_rename(&field.attrs)
                        .unwrap_or_else(|| rust_field_name.clone());

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
                        #(#filtered_attrs)*
                        #serde_default_attr
                        #schema_default_attr
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
                        #serde_default_attr
                        #schema_default_attr
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
    for (field_name, field_ty) in input.add.iter().flatten() {
        let field_ident: syn::Ident = syn::Ident::new(field_name, proc_macro2::Span::call_site());
        field_tokens.push(quote! {
            pub #field_ident: #field_ty
        });
    }

    // Build derive list
    // In multipart mode, force clone = false (FieldData<NamedTempFile> doesn't implement Clone)
    let derive_clone: bool = if input.multipart {
        false
    } else {
        input.derive_clone
    };
    let clone_derive: proc_macro2::TokenStream = if derive_clone {
        quote! { Clone, }
    } else {
        quote! {}
    };

    // Conditionally include Schema derive based on ignore_schema flag
    // Also generate #[schema(name = "...")] attribute if custom name is provided AND Schema is derived
    let schema_derive: proc_macro2::TokenStream;
    let schema_name_attr: proc_macro2::TokenStream;
    if input.ignore_schema {
        schema_derive = quote! {};
        schema_name_attr = quote! {};
    } else if let Some(ref name) = input.schema_name {
        schema_derive = quote! { vespera::Schema };
        schema_name_attr = quote! { #[schema(name = #name)] };
    } else {
        schema_derive = quote! { vespera::Schema };
        schema_name_attr = quote! {};
    }

    // Check if there are any relation fields
    let has_relation_fields = field_mappings.iter().any(|(_, _, _, is_rel)| *is_rel);

    // In multipart mode, skip From and from_model impls entirely
    let source_type: &syn::Type = &input.source_type;
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
    let generated_tokens: proc_macro2::TokenStream = if input.multipart {
        // Multipart mode: derive Multipart instead of serde
        // Emit #[serde(rename_all = ...)] so Multipart applies the rename at runtime
        // AND Schema derive reads it via extract_rename_all() fallback for OpenAPI field naming
        quote! {
            #(#inline_type_definitions)*

            #(#struct_doc_attrs)*
            #[derive(vespera::Multipart, #clone_derive #schema_derive)]
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

            // Same-file relation override helpers
            #(#relation_override_helpers)*

            // Default value functions for sea_orm(default_value) fields
            #(#default_functions)*

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
    let metadata = input.schema_name.as_ref().map(|custom_name| {
        // Build struct definition string for metadata (without derives/attrs for parsing)
        let struct_def = quote! {
            #[serde(rename_all = #effective_rename_all)]
            #(#serde_attrs_without_rename_all)*
            pub struct #new_type_name {
                #(#field_tokens),*
            }
        };
        StructMetadata::new(custom_name.clone(), struct_def.to_string())
    });

    Ok((generated_tokens, metadata))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
        StructMetadata::new(name.to_string(), definition.to_string())
    }

    fn to_storage(items: Vec<StructMetadata>) -> HashMap<String, StructMetadata> {
        items.into_iter().map(|s| (s.name.clone(), s)).collect()
    }

    #[test]
    fn test_generate_schema_type_code_multipart_with_add_and_custom_name() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "Upload",
            "pub struct Upload { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(
            UploadForm from Upload,
            multipart,
            name = "UploadFormSchema",
            add = [("extra": String)]
        );
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("vespera :: Multipart"));
        assert!(output.contains("extra"));
        assert!(output.contains("UploadFormSchema"));
        assert_eq!(metadata.unwrap().name, "UploadFormSchema");
    }
    // ============================================================
    // Tests for multipart mode
    // ============================================================

    #[test]
    fn test_generate_schema_type_code_multipart_basic() {
        // Tests: multipart mode generates Multipart derive, suppresses From impl
        let storage = to_storage(vec![create_test_struct_metadata(
            "UploadRequest",
            "pub struct UploadRequest { pub name: String, pub description: Option<String> }",
        )]);

        let tokens = quote!(PatchUpload from UploadRequest, multipart);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should derive Multipart
        assert!(output.contains("Multipart"));
        // Should NOT have From impl (multipart suppresses it)
        assert!(!output.contains("impl From"));
        // Should have the struct fields
        assert!(output.contains("name"));
        assert!(output.contains("description"));
    }

    #[test]
    fn test_generate_schema_type_code_multipart_with_rename() {
        // Tests: multipart mode with field rename
        let storage = to_storage(vec![create_test_struct_metadata(
            "UploadRequest",
            "pub struct UploadRequest { pub name: String, pub file_path: String }",
        )]);

        let tokens = quote!(RenamedUpload from UploadRequest, multipart, rename = [("file_path", "document_path")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should derive Multipart
        assert!(output.contains("Multipart"));
        // Should have renamed field
        assert!(output.contains("document_path"));
        // Original name should NOT appear as field
        assert!(!output.contains("file_path"));
    }

    #[test]
    fn test_generate_schema_type_code_multipart_with_form_data_attrs() {
        // Tests: multipart mode preserves #[form_data] attributes from source
        let storage = to_storage(vec![create_test_struct_metadata(
            "UploadRequest",
            r#"pub struct UploadRequest {
            pub name: String,
            #[form_data(limit = "10MiB")]
            pub file: String
        }"#,
        )]);

        let tokens = quote!(PatchUpload from UploadRequest, multipart);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should preserve form_data attributes
        assert!(output.contains("form_data"));
        assert!(output.contains("limit"));
    }

    #[test]
    fn test_generate_schema_type_code_multipart_skips_relations() {
        // Tests: multipart mode skips relation fields
        let storage = to_storage(vec![create_test_struct_metadata(
            "Model",
            r#"#[sea_orm(table_name = "memos")]
        pub struct Model {
            pub id: i32,
            pub title: String,
            pub user: BelongsTo<super::user::Entity>
        }"#,
        )]);

        let tokens = quote!(MemoUpload from Model, multipart);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Relation field should be skipped in multipart mode
        assert!(!output.contains("user"));
        // Regular fields should be present
        assert!(output.contains("id"));
        assert!(output.contains("title"));
        // Should derive Multipart
        assert!(output.contains("Multipart"));
    }

    #[test]
    fn test_generate_schema_type_code_multipart_partial() {
        // Coverage for multipart + partial combination
        let storage = to_storage(vec![create_test_struct_metadata(
            "UploadRequest",
            "pub struct UploadRequest { pub name: String, pub tags: String }",
        )]);

        let tokens = quote!(PatchUpload from UploadRequest, multipart, partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should derive Multipart
        assert!(output.contains("Multipart"));
        // Fields should be wrapped in Option (partial)
        assert!(output.contains("Option"));
        // Should NOT have From impl
        assert!(!output.contains("impl From"));
    }
}
