//! `schema_type!` code generation.
//!
//! Hosts `generate_schema_type_code` - the orchestrator that turns a
//! `SchemaTypeInput` (parsed `schema_type!` invocation) into the generated
//! struct, `From`/`from_model` impls, inline circular types, and metadata.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;

use super::defaults::generate_sea_orm_default_attrs;
use super::field_attrs::carried_field_attrs;
use super::file_cache;
use super::file_lookup::{
    find_struct_from_path_detailed, requested_module_path, storage_module_path,
};
use super::from_model::generate_from_model_with_relations;
use super::inline_types::{
    generate_inline_relation_type, generate_inline_relation_type_no_relations,
    generate_inline_type_definition,
};
use super::input::{PartialMode, SchemaTypeInput};
use super::module_scope::segment_ident as field_ident;
use super::same_file_override::maybe_generate_same_file_relation_override;
use super::seaorm::{
    RelationFieldInfo, convert_relation_type_to_schema_with_info, convert_type_with_chrono,
    extract_sea_orm_default_value, has_sea_orm_primary_key,
};
use super::transformation::{
    OptionWrap, RenameAll, build_omit_set, build_partial_config, build_pick_set, build_rename_map,
    container_serde_attrs, determine_rename_all, extract_doc_attrs, extract_form_data_attrs,
    field_form_name, field_serde_attrs, renamed_field_serde_rename, should_skip_field,
    should_wrap_in_option,
};
use super::type_utils::{
    extract_module_path, extract_type_name, is_option_type, is_seaorm_model,
    is_seaorm_relation_type,
};
use super::validation::{
    extract_source_field_names, validate_add_field_idents, validate_omit_fields,
    validate_partial_fields, validate_pick_fields, validate_rename_fields,
};
use crate::metadata::StructMetadata;
use crate::parser::strip_raw_prefix_owned;

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
    //
    // The storage-then-file-lookup resolution is identical for qualified
    // (`crate::models::user::Model`) and simple (`Model`) source paths, so a
    // single branch serves both and `find_struct_from_path_detailed` is called
    // exactly once and matched. The previous `else if let Ok(..) else match ..`
    // shape re-ran the full directory candidate scan + file parse a SECOND time
    // on a lookup miss purely to surface the error — wasted work that doubled
    // the cost of every "struct not found" compile error.
    //
    // When the struct is found via the file, the module path derived from the
    // actual file location overrides `source_module_path` so relative relation
    // paths like `super::user::Entity` resolve correctly (crucial for simple
    // names, more accurate than the parsed path for qualified ones).
    let struct_def_owned: StructMetadata;
    let schema_name_hint = input.schema_name.as_deref();
    //
    // Storage is keyed by the bare struct name, so a qualified source path
    // only trusts a storage entry whose defining file belongs to the
    // requested module (`crate::a::User` must not pick `crate::b::User`'s
    // derive); otherwise the file lookup decides, the storage entry being the
    // fallback when the file lookup cannot find the struct either.
    let storage_hit = schema_storage.get(&source_type_name);
    let storage_module = storage_hit.and_then(storage_module_path);
    let storage_trusted = match (requested_module_path(&input.source_type), &storage_module) {
        (Some(requested), Some(defined_in)) => &requested == defined_in,
        _ => true,
    };
    let struct_def = match storage_hit {
        Some(found) if storage_trusted => {
            // A bare storage hit carries no module path of its own: take the
            // defining file's so `super::`-relative relation paths resolve.
            if source_module_path.is_empty()
                && let Some(module) = storage_module
            {
                source_module_path = module;
            }
            found
        }
        _ => match find_struct_from_path_detailed(&input.source_type, schema_name_hint) {
            Ok((found, module_path)) => {
                struct_def_owned = found;
                source_module_path = module_path;
                &struct_def_owned
            }
            Err(err) => storage_hit.ok_or_else(|| err.to_syn_error(&input.source_type))?,
        },
    };

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = file_cache::parse_struct_cached(&struct_def.definition)
        .map_err(|e| {
            syn::Error::new_spanned(
                &input.source_type,
                format!("failed to parse struct definition for `{source_type_name}`: {e}"),
            )
        })?;
    // A tuple / unit source has no field names to pick, rename or carry:
    // the DTO used to come out empty (and its `From` dropped every value).
    if !matches!(parsed_struct.fields, syn::Fields::Named(_)) {
        return Err(syn::Error::new_spanned(
            &input.source_type,
            format!(
                "schema_type!: `{source_type_name}` is a tuple or unit struct; schema_type! derives from structs with named fields"
            ),
        ));
    }

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
    // `add` field names also become struct identifiers via `syn::Ident::new`
    // downstream, so reject a non-identifier / keyword name here as a spanned
    // error instead of panicking the proc-macro during expansion.
    validate_add_field_idents(input.add.as_ref(), &input.source_type)?;
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

    // Container serde attributes from the source struct, rebuilt key by key
    // without rename_all (emitted separately) and the impl-dependent keys.
    let serde_attrs_without_rename_all = container_serde_attrs(
        &parsed_struct.attrs,
        &strip_raw_prefix_owned(parsed_struct.ident.to_string()),
    );

    // Extract doc comments from source struct to carry over to generated struct
    let struct_doc_attrs = extract_doc_attrs(&parsed_struct.attrs);

    // Determine the effective rename_all strategy
    let effective_rename_all =
        determine_rename_all(input.rename_all.as_ref(), &parsed_struct.attrs);
    let rename_all = RenameAll::new(
        input.rename_all.as_ref(),
        &parsed_struct.attrs,
        &effective_rename_all,
    );
    let rename_all_attr = rename_all.attr();

    // Check if source is a SeaORM Model
    let is_source_seaorm_model = is_seaorm_model(&parsed_struct);

    // Generate new struct with filtered fields
    let new_type_name = &input.new_type;
    // Pre-size the two dense per-field codegen vectors from the source field
    // count so a many-field struct fills them without the Vec's early doubling
    // reallocations (the previous `Vec::new()` reallocated at 1, 2, 4, 8, ...).
    // The sparser relation / inline / default / override vectors below stay
    // `Vec::new()`: they hold only the subset of fields that are relations or
    // carry SeaORM defaults, so sizing them to the full field count would
    // over-allocate for the common struct that has neither.
    let field_capacity = match &parsed_struct.fields {
        syn::Fields::Named(fields_named) => fields_named.named.len(),
        _ => 0,
    };
    let mut field_tokens = Vec::with_capacity(field_capacity);
    // Track field mappings for From impl: (new_field_ident, source_field_ident, wrapped_in_option, is_relation)
    let mut field_mappings: Vec<(syn::Ident, syn::Ident, bool, bool)> =
        Vec::with_capacity(field_capacity);
    // Track relation field info for from_model generation
    let mut relation_fields: Vec<RelationFieldInfo> = Vec::new();
    // Track inline types that need to be generated for circular relations
    let mut inline_type_definitions: Vec<TokenStream> = Vec::new();
    // Track default value functions generated from sea_orm(default_value)
    let mut default_functions: Vec<TokenStream> = Vec::new();
    // Track same-file relation override helpers
    let mut relation_override_helpers: Vec<TokenStream> = Vec::new();
    // Track serde codec adapters for `partial` fields with `with` & co.
    let mut codec_adapters: Vec<TokenStream> = Vec::new();
    // The same fields as `field_tokens`, but always with their `#[schema]`
    // attributes: the registered metadata feeds the OpenAPI document even
    // when `ignore` leaves the emitted struct without a `Schema` derive.
    let mut metadata_field_tokens: Vec<TokenStream> = Vec::with_capacity(field_capacity);
    // `#[cfg(..)]` of each source field (by source name), repeated on its
    // `From` / `from_model` assignment.
    let mut field_cfgs: HashMap<String, TokenStream> = HashMap::new();
    let schema_derived = !input.ignore_schema;
    // A source field with `#[garde(..)]` makes the DTO derive
    // `garde::Validate`; every other field then needs `#[garde(skip)]`.
    let any_garde = parsed_struct
        .fields
        .iter()
        .any(|field| carried_field_attrs(&field.attrs, schema_derived).garde);

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
            // `T` of a field `partial` wrapped into `Option<T>`.
            let mut wrapped_inner_ty: Option<TokenStream> = None;

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
                                // Only reachable for a field the `pick` guard
                                // above already required, so the failure is
                                // always an error - never a silent skip.
                                return Err(syn::Error::new_spanned(
                                    field,
                                    format!(
                                        "schema_type!: relation field `{rust_field_name}` was explicitly picked but its inline relation type could not be generated"
                                    ),
                                ));
                            }
                        } else {
                            // BelongsTo/HasOne: Include by default
                            if let Some((_, adapter_name)) = input
                                .relation_adapters
                                .iter()
                                .find(|(field_name, _)| field_name == &rust_field_name)
                            {
                                let (override_field_ty, helper_tokens) =
                                    maybe_generate_same_file_relation_override(
                                        new_type_name,
                                        &rust_field_name,
                                        adapter_name,
                                        &rel_info,
                                        schema_storage,
                                    )?;
                                relation_override_helpers.push(helper_tokens);
                                rel_info.adapter = true;
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
                        if pick_set.contains(&rust_field_name) {
                            return Err(syn::Error::new_spanned(
                                field,
                                format!(
                                    "schema_type!: relation field `{rust_field_name}` was explicitly picked but its SeaORM relation type could not be converted"
                                ),
                            ));
                        }
                        // Fallback: skip if conversion fails
                        continue;
                    }
                } else {
                    // Convert SeaORM datetime types to chrono equivalents
                    // Also resolves local types to absolute paths
                    let converted_ty = convert_type_with_chrono(original_ty, &source_module_path);
                    if should_wrap_option {
                        wrapped_inner_ty = Some(converted_ty.clone());
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
            let carried = carried_field_attrs(&field.attrs, schema_derived);
            if !carried.cfgs.is_empty() {
                let cfgs = &carried.cfgs;
                field_cfgs.insert(rust_field_name.clone(), quote! { #(#cfgs)* });
            }
            let carried_schema = &carried.schema;
            let carried_other = &carried.other;
            let garde_skip = (any_garde && !carried.garde).then(|| quote! { #[garde(skip)] });
            // `#[schema]` only where `Schema` is derived (a bare `#[schema]`
            // is an unknown attribute otherwise); the metadata keeps them.
            let (emitted_schema, mut metadata_only) = if schema_derived {
                (quote! { #(#carried_schema)* }, TokenStream::new())
            } else {
                (TokenStream::new(), quote! { #(#carried_schema)* })
            };
            let carried_attrs = quote! { #emitted_schema #(#carried_other)* #garde_skip };

            if input.multipart {
                // Multipart mode: emit form_data attrs, suppress serde attrs
                let form_data_attrs = extract_form_data_attrs(&field.attrs);

                // Check if field should be renamed (rename still applies to Rust field names)
                if let Some(new_name) = rename_map.get(&rust_field_name) {
                    let new_field_ident = field_ident(new_name);
                    // The form field keeps the source's name.
                    let form_name_attr = field_form_name(
                        &field.attrs,
                        &rust_field_name,
                        &effective_rename_all,
                        true,
                    );

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
                        #carried_attrs
                        #(#form_data_attrs)*
                        #form_name_attr
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
                    // A source `#[serde(rename)]` keeps naming the form field.
                    let form_name_attr = field_form_name(
                        &field.attrs,
                        &rust_field_name,
                        &effective_rename_all,
                        false,
                    );

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
                        #carried_attrs
                        #(#form_data_attrs)*
                        #form_name_attr
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
                let is_renamed = rename_map.contains_key(&rust_field_name);
                let adapter_prefix = format!("__vespera_{new_type_name}_{rust_field_name}");
                let wrap = wrapped_inner_ty.as_ref().map(|inner_ty| OptionWrap {
                    adapter_prefix: &adapter_prefix,
                    inner_ty,
                });
                let (serde_field_attrs, adapters) =
                    field_serde_attrs(&field.attrs, wrap.as_ref(), is_renamed);
                codec_adapters.push(adapters);

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
                // The source's own `#[schema(default ..)]` wins; without a
                // `Schema` derive the attribute only goes to the metadata.
                let schema_default_attr = if carried.schema_default {
                    TokenStream::new()
                } else if schema_derived {
                    schema_default_attr
                } else {
                    metadata_only.extend(schema_default_attr);
                    TokenStream::new()
                };

                // Check if field should be renamed
                if let Some(new_name) = rename_map.get(&rust_field_name) {
                    // Create new identifier for the field
                    let new_field_ident: syn::Ident = field_ident(new_name);

                    // The JSON key stays the source's: its explicit serde
                    // rename, else rename_all applied to the SOURCE name
                    // (`serde_field_attrs` already dropped `rename`).
                    let rename_attr =
                        renamed_field_serde_rename(&field.attrs, &rust_field_name, &rename_all);

                    field_tokens.push(quote! {
                        #(#doc_attrs)*
                        #carried_attrs
                        #(#serde_field_attrs)*
                        #serde_default_attr
                        #schema_default_attr
                        #rename_attr
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
                        #carried_attrs
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
            let emitted = field_tokens.last().cloned().unwrap_or_default();
            metadata_field_tokens.push(quote! { #metadata_only #emitted });
        }
    }

    // Add new fields from `add` parameter
    let add_garde_skip = any_garde.then(|| quote! { #[garde(skip)] });
    for (field_name, field_ty) in input.add.iter().flatten() {
        let field_ident = field_ident(field_name);
        let field = quote! {
            #add_garde_skip
            pub #field_ident: #field_ty
        };
        field_tokens.push(field.clone());
        metadata_field_tokens.push(field);
    }

    // Build derive list
    // In multipart mode, force clone = false (FieldData<NamedTempFile> doesn't implement Clone)
    let derive_clone: bool = if input.multipart {
        false
    } else {
        input.derive_clone
    };
    // Source `#[garde(..)]` rules come with the derive that reads them
    // (and the source container's own `#[garde(..)]`, e.g. `context`).
    let garde_derive = any_garde.then(|| quote! { ::garde::Validate, });
    let container_garde_attrs: Vec<&syn::Attribute> = if any_garde {
        parsed_struct
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("garde"))
            .collect()
    } else {
        Vec::new()
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
                    let cfg = field_cfgs.get(&strip_raw_prefix_owned(source_ident.to_string()));
                    if *wrapped {
                        quote! { #cfg #new_ident: Some(source.#source_ident) }
                    } else {
                        quote! { #cfg #new_ident: source.#source_ident }
                    }
                })
                .collect();

            quote! {
                #[allow(deprecated)]
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
                    &field_cfgs,
                )?
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
            #[derive(vespera::Multipart, #garde_derive #clone_derive #schema_derive)]
            #(#container_garde_attrs)*
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

            // Serde codec adapters for `partial` fields with `with` & co.
            #(#codec_adapters)*

            #(#struct_doc_attrs)*
            #[derive(serde::Serialize, serde::Deserialize, #garde_derive #clone_derive #schema_derive)]
            #(#container_garde_attrs)*
            #schema_name_attr
            #rename_all_attr
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
            #rename_all_attr

            #(#serde_attrs_without_rename_all)*
            pub struct #new_type_name {
                #(#metadata_field_tokens),*
            }
        };
        StructMetadata::new(custom_name.clone(), struct_def.to_string())
    });

    Ok((generated_tokens, metadata))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_field_attrs;
