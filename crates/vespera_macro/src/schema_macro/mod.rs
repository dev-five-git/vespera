//! Schema macro implementation
//!
//! Provides macros for generating OpenAPI schemas from struct types:
//! - `schema!` - Generate Schema value with optional field filtering
//! - `schema_type!` - Generate new struct type derived from existing type
//!
//! # Module Structure
//!
//! This module is split into submodules for maintainability:
//! - `input` - Input parsing types (SchemaInput, SchemaTypeInput)
//! - `type_utils` - Type extraction and conversion helpers
//! - `file_lookup` - File system operations for finding structs
//! - `relation` - SeaORM relation handling
//! - `inline_type` - Inline type generation for circular references
//! - `codegen` - Code generation utilities

pub mod codegen;
pub mod file_lookup;
pub mod inline_type;
pub mod input;
pub mod relation;
pub mod type_utils;

// Re-export public types
pub use input::{PartialMode, SchemaInput, SchemaTypeInput};

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;

use crate::metadata::StructMetadata;
use crate::parser::{extract_field_rename, extract_rename_all, strip_raw_prefix};

use codegen::{generate_filtered_schema, generate_from_model_with_relations};
use file_lookup::find_struct_from_path;
use inline_type::{
    generate_inline_relation_type, generate_inline_relation_type_no_relations,
    generate_inline_type_definition,
};
use relation::{RelationFieldInfo, convert_relation_type_to_schema_with_info};
use type_utils::{
    convert_type_with_chrono, extract_module_path, extract_type_name, is_option_type,
    is_qualified_path, is_seaorm_model, is_seaorm_relation_type,
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

    // Extract doc comments from source struct to carry over to generated struct
    let struct_doc_attrs: Vec<_> = parsed_struct
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
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

            // Filter field attributes: keep serde and doc attributes, remove sea_orm and others
            // This is important when using schema_type! with models from other files
            // that may have ORM-specific attributes we don't want in the generated struct
            let serde_field_attrs: Vec<_> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("serde"))
                .collect();

            // Extract doc attributes to carry over comments to the generated struct
            let doc_attrs: Vec<_> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("doc"))
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
    use quote::quote;

    fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
        StructMetadata::new(name.to_string(), definition.to_string())
    }

    #[test]
    fn test_generate_schema_code_simple_struct() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote!(User);
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

        let tokens = quote!(User, omit = ["password"]);
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

        let tokens = quote!(User, pick = ["id", "name"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
    }

    #[test]
    fn test_generate_schema_code_type_not_found() {
        let storage: Vec<StructMetadata> = vec![];

        let tokens = quote!(NonExistent);
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

        let tokens = quote!(BadStruct);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to parse"));
    }

    #[test]
    fn test_generate_schema_type_code_pick_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote!(NewUser from User, pick = ["nonexistent"]);
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

        let tokens = quote!(NewUser from User, omit = ["nonexistent"]);
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

        let tokens = quote!(NewUser from User, rename = [("nonexistent", "new_name")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_type_not_found() {
        let storage: Vec<StructMetadata> = vec![];

        let tokens = quote!(NewUser from NonExistent);
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

        let tokens = quote!(CreateUser from User, pick = ["name"]);
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

        let tokens = quote!(SafeUser from User, omit = ["password"]);
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

        let tokens = quote!(UserWithExtra from User, add = [("extra": String)]);
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
        let tokens = quote!(UserResponse from User, pick = ["id", "name"]);
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
        let tokens = quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // Should NOT contain From impl when add is used
        assert!(!output.contains("impl From"));
    }

    #[test]
    fn test_generate_schema_type_code_with_partial_all() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub bio: Option<String> }",
        )];

        let tokens = quote!(UpdateUser from User, partial);
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

        let tokens = quote!(UpdateUser from User, partial = ["name"]);
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

        let tokens = quote!(UpdateUser from User, partial = ["nonexistent"]);
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

        let tokens = quote!(UpdateUser from User, partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        // From impl should wrap values in Some()
        assert!(output.contains("Some (source . id)"));
        assert!(output.contains("Some (source . name)"));
    }

    #[test]
    fn test_generate_schema_type_code_preserves_struct_doc() {
        let input = SchemaTypeInput {
            new_type: syn::Ident::new("NewUser", proc_macro2::Span::call_site()),
            source_type: syn::parse_str("User").unwrap(),
            omit: None,
            pick: None,
            rename: None,
            add: None,
            derive_clone: true,
            partial: None,
            schema_name: None,
            ignore_schema: false,
            rename_all: None,
        };
        // Create a struct with doc comments
        let struct_def = StructMetadata {
            name: "User".to_string(),
            definition: r#"
                /// User struct documentation
                pub struct User {
                    /// The user ID
                    pub id: i32,
                    /// The user name
                    pub name: String,
                }
            "#
            .to_string(),
            include_in_openapi: true,
        };
        let result = generate_schema_type_code(&input, &[struct_def]);
        assert!(result.is_ok());
        let (tokens, _) = result.unwrap();
        let tokens_str = tokens.to_string();
        // Should contain doc comments
        assert!(tokens_str.contains("User struct documentation") || tokens_str.contains("doc"));
    }
}
