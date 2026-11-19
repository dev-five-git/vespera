//! `schema_type!` code generation.
//!
//! Hosts `generate_schema_type_code` - the orchestrator that turns a
//! `SchemaTypeInput` (parsed `schema_type!` invocation) into the generated
//! struct, `From`/`from_model` impls, inline circular types, and metadata.

use std::collections::{HashMap, HashSet};

use proc_macro2::TokenStream;
use quote::quote;

use super::defaults::{gate_sea_orm_defaults, generate_sea_orm_default_attrs_in};
use super::field_attrs::carried_field_attrs;
use super::file_cache;
use super::file_lookup::call_site_module;
use super::from_model::{
    FieldMapping, generate_from_model_with_relations, source_column_read, source_field_named,
};
use super::garde_paths::reroot_garde_attr;
use super::garde_peers::{PeerContext, PeerRef, check_garde_peers};
use super::inline_types::{
    generate_inline_relation_type, generate_inline_relation_type_no_relations,
    generate_inline_type_definition,
};
use super::input::{PartialMode, SchemaTypeInput};
use super::instantiate::{instantiate_with_bindings, self_path};
use super::module_scope::segment_ident as field_ident;
use super::same_file_override::maybe_generate_same_file_relation_override;
use super::seaorm::{
    RelationFieldInfo, convert_relation_type_to_schema_with_info, convert_type_with_chrono,
    has_sea_orm_database_default, has_sea_orm_primary_key, primary_key_count,
};
use super::transformation::{
    OptionWrap, build_partial_config, build_rename_map, container_serde_attrs, dto_renaming,
    extract_doc_attrs, extract_form_data_attrs, field_form_name, field_serde_attrs,
    renamed_field_serde_rename, reroot_serde_paths, should_skip_field, should_wrap_in_option,
};
use super::type_utils::{
    extract_module_path, extract_type_name, is_option_type, is_seaorm_model,
    is_seaorm_relation_type,
};
use super::validation::{
    SpannedSource, extract_source_field_names, resolve_filter_names, validate_add_field_idents,
    validate_included, validate_partial_fields, validate_relation_adapters, validate_rename_fields,
};
use crate::metadata::StructMetadata;
use crate::parser::schema::SerdeSide;
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
    // The `name = ..` override is a `components.schemas` key.
    if let Some(name) = input.schema_name.as_deref() {
        crate::schema_impl::require_component_key(
            name,
            syn::spanned::Spanned::span(&input.source_type),
        )?;
    }

    // SCHEMA_STORAGE first (no file I/O), else a file lookup (e.g. a SeaORM
    // Model); the defining module overrides the path as written.
    let (struct_def, defined_in) =
        resolve::resolve_source(input, &source_type_name, schema_storage)?;
    let source_module_path = defined_in.unwrap_or_else(|| extract_module_path(&input.source_type));

    // Parse the struct definition
    let mut parsed_struct: syn::ItemStruct =
        file_cache::parse_struct_cached(&struct_def.definition).map_err(|e| {
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
    super::instantiate::name_self_in_types(&mut parsed_struct);
    // A generic source takes the call's type arguments (`Page<User>`).
    let generic_fields: Vec<syn::Type> = parsed_struct
        .fields
        .iter()
        .map(|field| field.ty.clone())
        .collect();
    let (mut parsed_struct, bindings) =
        instantiate_with_bindings(&parsed_struct, &input.source_type, "schema_type!")?;
    let self_path = self_path(&parsed_struct.ident, bindings.as_ref(), &source_module_path);
    super::transformation::name_self_in_paths(&mut parsed_struct, &self_path);
    super::field_attrs::hoist_cfg_attr_cfgs(&mut parsed_struct);

    // Extract all field names from source struct for validation
    // Include relation fields since they can be converted to Schema types
    let source_field_names = extract_source_field_names(&parsed_struct);

    // Validate all field references exist in source struct; a diagnostic
    // points at the offending name's literal.
    let at = SpannedSource {
        source_type: &input.source_type,
        spans: &input.spans,
    };
    // `pick` / `omit` names resolve as in `schema!` (Rust identifier first).
    let pick_set = resolve_filter_names(
        "pick",
        input.pick.as_ref(),
        &parsed_struct,
        &source_type_name,
        &at,
    )?;
    let omit_set = resolve_filter_names(
        "omit",
        input.omit.as_ref(),
        &parsed_struct,
        &source_type_name,
        &at,
    )?;
    validate_rename_fields(
        input.rename.as_ref(),
        &source_field_names,
        &at,
        &source_type_name,
    )?;
    validate_relation_adapters(
        &input.relation_adapters,
        &source_field_names,
        &at,
        &source_type_name,
    )?;
    // `add` field names also become struct identifiers via `syn::Ident::new`
    // downstream, so reject a non-identifier / keyword name here as a spanned
    // error instead of panicking the proc-macro during expansion.
    validate_add_field_idents(input.add.as_ref(), &at)?;
    let partial_fields_to_validate = match &input.partial {
        Some(PartialMode::Fields(fields)) => Some(fields),
        _ => None,
    };
    validate_partial_fields(
        partial_fields_to_validate,
        &source_field_names,
        &at,
        &source_type_name,
    )?;

    // Build filter sets and rename map
    validate_included(
        "rename",
        input.rename.iter().flatten().map(|(from, _)| from.as_str()),
        &omit_set,
        &pick_set,
        &at,
    )?;
    validate_included(
        "partial",
        partial_fields_to_validate
            .into_iter()
            .flatten()
            .map(String::as_str),
        &omit_set,
        &pick_set,
        &at,
    )?;
    validate_included(
        "relation_adapters",
        input
            .relation_adapters
            .iter()
            .map(|(field, _)| field.as_str()),
        &omit_set,
        &pick_set,
        &at,
    )?;
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
    // (`multipart`: the deserialize side, which the `Multipart` derive reads).
    let rename_side = if input.multipart {
        SerdeSide::Deserialize
    } else {
        SerdeSide::Serialize
    };
    // Raw Rust field names for a `multipart` DTO of a source without a
    // deserialize-side `rename_all` (R23-CJ1-M1, see `dto_renaming`).
    let renaming = dto_renaming(
        input.rename_all.as_ref(),
        input.multipart,
        &parsed_struct.attrs,
        rename_side,
    );
    let multipart_raw_names = renaming.raw_names;
    let effective_rename_all = renaming.effective;
    let rename_all = renaming.rename_all;
    let rename_all_attr = rename_all.attr();

    // Check if source is a SeaORM Model
    let is_source_seaorm_model = is_seaorm_model(&parsed_struct);

    // Generate new struct with filtered fields
    let new_type_name = &input.new_type;
    // Pre-size the two dense per-field codegen vectors from the source field
    // count (the sparser relation / inline / default / override vectors stay
    // `Vec::new()`: most structs have neither).
    let field_capacity = parsed_struct.fields.len();
    let mut field_tokens = Vec::with_capacity(field_capacity);
    // The source field behind every emitted field (`From`, `from_model`).
    let mut field_mappings: Vec<FieldMapping> = Vec::with_capacity(field_capacity);
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
    let schema_derived = !input.ignore_schema;
    // A source field with `#[garde(..)]` makes the DTO derive
    // `garde::Validate`; every other field then needs `#[garde(skip)]`.
    // Only a direct `#[garde]` or one inside a `cfg_attr` sets it, so the
    // full attribute carry (clone, quote, `cfg_attr` parse) runs only for a
    // field with a `cfg_attr`.
    let any_garde = parsed_struct.fields.iter().any(|field| {
        field.attrs.iter().any(|attr| attr.path().is_ident("garde"))
            || (field
                .attrs
                .iter()
                .any(|attr| attr.path().is_ident("cfg_attr"))
                && carried_field_attrs(&field.attrs, schema_derived, &source_module_path, false)
                    .garde)
    });
    // A carried `#[deprecated]` field: the struct (and so its serde
    // derives) `#[allow(deprecated)]`.
    let mut any_deprecated = false;
    // The `relation_adapters` entries a HasOne / BelongsTo field used.
    let mut used_adapters: HashSet<String> = HashSet::new();
    // Source fields `omit` / `pick` keep that the type still does not emit,
    // with the reason (`rename` / `partial` / `pick` naming one is an
    // error, not a silent no-op).
    let mut dropped: HashMap<String, &'static str> = HashMap::new();
    let mut relation_names: HashSet<String> = HashSet::new();
    // The sibling fields the carried garde rules name (checked once every
    // emitted field is known).
    let mut garde_peer_refs: Vec<PeerRef> = Vec::new();
    // `#[cfg]` twins (one name, several fields): their generated helpers
    // are named by field index, the names would collide otherwise.
    let twin_names: HashSet<String> = {
        let mut seen = HashSet::new();
        parsed_struct
            .fields
            .iter()
            .filter_map(|field| field.ident.as_ref())
            .map(|ident| strip_raw_prefix_owned(ident.to_string()))
            .filter(|name| !seen.insert(name.clone()))
            .collect()
    };
    // The source fields `partial` wraps in `Option` (a wrapped field's garde
    // `matches(other)` then compares two `Option`s).
    let wrapped_fields: HashSet<String> = (parsed_struct.fields.iter())
        .filter_map(|field| field.ident.as_ref().map(|ident| (field, ident)))
        .map(|(field, ident)| (field, strip_raw_prefix_owned(ident.to_string())))
        .filter(|(field, name)| {
            let (option, relation) = (
                is_option_type(&field.ty),
                is_seaorm_relation_type(&field.ty),
            );
            should_wrap_in_option(name, partial_all, &partial_set, option, relation)
        })
        .map(|(_, name)| name)
        .collect();
    // The module the DTO is emitted in (`None` unknown): a `From` reading a
    // field it cannot see is not emitted.
    let call_site = call_site_module();
    let mut all_readable = true;
    // A source container default fills a missing `Vec` part itself (the
    // Multipart derive reads the container's value, not `[]`).
    let container_default = crate::parser::extract_default(&parsed_struct.attrs).is_some();
    // Multipart bare `Vec` fields left to that container default: their
    // `#[form_data(default)]` twin (emitted, metadata), used when the DTO
    // cannot carry the container default (`add`, relations, unreadable
    // fields) and the parts would otherwise read as required.
    let mut container_vec_fields: Vec<(usize, TokenStream, TokenStream)> = Vec::new();

    if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
        let pk_count = primary_key_count(&parsed_struct.fields);
        for (field_index, field) in fields_named.named.iter().enumerate() {
            let rust_field_name = field.ident.as_ref().map_or_else(
                || "unknown".to_string(),
                |i| strip_raw_prefix_owned(i.to_string()),
            );

            // Apply omit/pick filters
            if should_skip_field(&rust_field_name, &omit_set, &pick_set) {
                continue;
            }

            // Apply omit_default: skip fields with sea_orm(default_value / default_expr) or sea_orm(primary_key)
            if input.omit_default
                && (has_sea_orm_database_default(&field.attrs)
                    || has_sea_orm_primary_key(&field.attrs, &field.ty, pk_count))
            {
                dropped.insert(
                    rust_field_name,
                    "`omit_default` drops it (a SeaORM default / primary key)",
                );
                continue;
            }

            // Check if this is a SeaORM relation type
            let is_relation = is_seaorm_relation_type(&field.ty);

            // In multipart mode, skip ALL relation fields (multipart forms can't represent nested objects)
            if input.multipart && is_relation {
                dropped.insert(rust_field_name, "`multipart` types carry no relations");
                continue;
            }

            if is_relation {
                relation_names.insert(rust_field_name.clone());
            }
            // The field's `#[cfg]`, repeated on its helpers and assignments,
            // and the name its helpers are generated under.
            let field_cfg = assignments::field_cfg(&field.attrs);
            let helper_key = if twin_names.contains(&rust_field_name) {
                format!("{rust_field_name}_{field_index}")
            } else {
                rust_field_name.clone()
            };
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
                                dropped.insert(
                                    rust_field_name,
                                    "a HasMany relation is only included when `pick`ed",
                                );
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
                                inline_type_definitions
                                    .push(assignments::gate_items(&field_cfg, inline_type_def));

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
                                        input.derive_clone,
                                    )?;
                                relation_override_helpers
                                    .push(assignments::gate_items(&field_cfg, helper_tokens));
                                rel_info.adapter = true;
                                used_adapters.insert(rust_field_name.clone());
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
                                inline_type_definitions
                                    .push(assignments::gate_items(&field_cfg, inline_type_def));

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
                        dropped.insert(
                            rust_field_name,
                            "its SeaORM relation type could not be converted",
                        );
                        continue;
                    }
                } else {
                    // Convert SeaORM datetime types to chrono equivalents
                    // Also resolves local types to absolute paths
                    // A generic source's field resolves before its type
                    // arguments are inserted: they are written at the call
                    // site, not in the source module.
                    let converted_ty = bindings.as_ref().map_or_else(
                        || convert_type_with_chrono(original_ty, &source_module_path),
                        |bindings| {
                            bindings.field_type(&generic_fields[field_index], &source_module_path)
                        },
                    );
                    if should_wrap_option {
                        wrapped_inner_ty = Some(converted_ty.clone());
                        (Box::new(quote! { Option<#converted_ty> }), None)
                    } else {
                        (Box::new(converted_ty), None)
                    }
                };

            // Collect relation info (paired with its field by position).
            let relation = relation_info.as_ref().map(|_| relation_fields.len());
            relation_fields.extend(relation_info);
            // `pub(super)` / `pub(in ..)` name source-relative modules.
            let vis = assignments::dto_visibility(&field.vis);
            all_readable &= relation.is_some()
                || assignments::readable_from(
                    &field.vis,
                    &source_module_path,
                    call_site.as_deref(),
                );
            let source_field_ident: syn::Ident = field.ident.clone().unwrap();
            // A `rename`d field gets the new name; the wire name stays.
            let new_name = rename_map.get(&rust_field_name);
            let dto_field_ident =
                new_name.map_or_else(|| source_field_ident.clone(), |name| field_ident(name));

            // Extract doc attributes to carry over comments to the generated struct
            let doc_attrs = extract_doc_attrs(&field.attrs);
            let renamed = new_name.is_some();
            let mut carried =
                carried_field_attrs(&field.attrs, schema_derived, &source_module_path, renamed);
            if should_wrap_option {
                carried = carried.for_option_wrap(&|name| wrapped_fields.contains(name));
            }
            if carried.garde {
                let peers = carried.rename_garde_peers(&rename_map).into_iter();
                garde_peer_refs.extend(peers.map(|(peer, matches)| PeerRef {
                    field: rust_field_name.clone(),
                    field_wrapped: should_wrap_option,
                    peer,
                    matches,
                }));
            }
            any_deprecated |= carried.deprecated;
            let cfg_serde = carried.cfg_serde_attrs();
            let carried_schema = &carried.schema;
            let carried_other = &carried.other;
            let garde_skip = (any_garde && !carried.garde).then(|| quote! { #[garde(skip)] });
            // `#[schema]` only where `Schema` is derived (a bare `#[schema]`
            // is an unknown attribute otherwise); the metadata keeps them,
            // a `cfg_attr` one when its predicate is known to hold.
            let (emitted_schema, mut metadata_only) = if schema_derived {
                let cfg_schema = carried.cfg_schema_attrs();
                (
                    quote! { #(#carried_schema)* #(#cfg_schema)* },
                    TokenStream::new(),
                )
            } else {
                let active = carried.active_cfg_schema();
                (
                    TokenStream::new(),
                    quote! { #(#carried_schema)* #(#active)* },
                )
            };
            let carried_attrs = quote! { #emitted_schema #(#carried_other)* #garde_skip };
            let mut container_vec_twin = None;

            if input.multipart {
                // Multipart mode: emit form_data attrs, suppress serde attrs
                let form_data_attrs = extract_form_data_attrs(&field.attrs);
                // The serde missing-field keys the `Multipart` derive honours.
                let serde_carry = multipart_carry::field_attr(
                    &field.attrs,
                    &source_module_path,
                    should_wrap_option,
                );
                let form_cfg_serde = carried.multipart_cfg_serde_attrs();
                // A bare `Vec` reads "no parts" as `[]`: the request schema
                // must not list it as required (a hand-written Multipart
                // struct gets the same in its stored definition).
                let bare_vec = !should_wrap_option
                    && crate::multipart_impl::is_vec_type(original_ty)
                    && crate::parser::extract_default(&field.attrs).is_none()
                    && !crate::multipart_impl::has_form_data_default(&field.attrs);
                let vec_default =
                    (bare_vec && !container_default).then(|| quote! { #[form_data(default)] });
                // The form field keeps the source's name (a source
                // `#[serde(rename)]` keeps naming it).
                let form_name_attr = field_form_name(
                    &field.attrs,
                    &rust_field_name,
                    &effective_rename_all,
                    renamed,
                );

                let field = |vec_default: Option<TokenStream>| {
                    quote! {
                        #(#doc_attrs)*
                        #carried_attrs
                        #(#form_data_attrs)*
                        #vec_default
                        #serde_carry
                        #(#form_cfg_serde)*
                        #form_name_attr
                        #vis #dto_field_ident: #field_ty
                    }
                };
                if bare_vec && container_default {
                    container_vec_twin = Some(field(Some(quote! { #[form_data(default)] })));
                }
                field_tokens.push(field(vec_default));
            } else {
                // Normal (serde) mode: keep serde and doc attributes, drop
                // sea_orm and others (a model of another file may carry
                // ORM-specific attributes the DTO must not).
                let adapter_prefix = format!(
                    "__vespera_{}_{helper_key}",
                    strip_raw_prefix_owned(new_type_name.to_string())
                );
                let wrap = wrapped_inner_ty.as_ref().map(|inner_ty| OptionWrap {
                    adapter_prefix: &adapter_prefix,
                    inner_ty,
                });
                let (serde_field_attrs, adapters) = field_serde_attrs(
                    &reroot_serde_paths(&field.attrs, &source_module_path),
                    wrap.as_ref(),
                    renamed,
                );
                codec_adapters.push(assignments::gate_items(&field_cfg, adapters));

                // Generate serde default + schema(default) from sea_orm(default_value) or primary_key
                // Handles literal defaults, SQL function defaults, and implicit auto-increment
                let mut field_defaults = Vec::new();
                let generated = generate_sea_orm_default_attrs_in(
                    &field.attrs,
                    new_type_name,
                    &helper_key,
                    original_ty,
                    &field_ty,
                    should_wrap_option || is_option_type(original_ty),
                    pk_count,
                    &source_module_path,
                    &mut field_defaults,
                );
                default_functions.extend(
                    super::defaults::allow_unused_default_fns(field_defaults, &carried)
                        .into_iter()
                        .map(|function| assignments::gate_items(&field_cfg, function)),
                );
                let (serde_default_attr, schema_default_attr) =
                    gate_sea_orm_defaults(generated, &carried, schema_derived);
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

                // The JSON key of a renamed field stays the source's: its
                // explicit serde rename, else rename_all applied to the
                // SOURCE name (`serde_field_attrs` already dropped `rename`).
                let rename_attr = new_name.map(|_| {
                    renamed_field_serde_rename(&field.attrs, &rust_field_name, &rename_all)
                });

                field_tokens.push(quote! {
                    #(#doc_attrs)*
                    #carried_attrs
                    #(#serde_field_attrs)*
                    #(#cfg_serde)*
                    #serde_default_attr
                    #schema_default_attr
                    #rename_attr
                    #vis #dto_field_ident: #field_ty
                });
            }
            field_mappings.push(FieldMapping {
                new_ident: dto_field_ident,
                source_ident: source_field_ident,
                wrapped: should_wrap_option,
                relation,
                cfg: field_cfg,
            });
            let emitted = field_tokens.last().cloned().unwrap_or_default();
            metadata_field_tokens.push(quote! { #metadata_only #emitted });
            if let Some(twin) = container_vec_twin {
                let metadata = quote! { #metadata_only #twin };
                container_vec_fields.push((field_tokens.len() - 1, twin, metadata));
            }
        }
    }
    checks::check_emitted(
        input,
        partial_fields_to_validate,
        &checks::Emitted {
            dropped: &dropped,
            relation_names: &relation_names,
            used_adapters: &used_adapters,
            field_mappings: &field_mappings,
            picked: &pick_set,
        },
        &at,
        &source_type_name,
    )?;
    let emitted: HashSet<String> = (field_mappings.iter())
        .map(|mapping| strip_raw_prefix_owned(mapping.source_ident.to_string()))
        .collect();
    let peer_context = PeerContext {
        source_fields: &source_field_names,
        emitted: &emitted,
        omit: &omit_set,
        pick: &pick_set,
        dropped: &dropped,
        wrapped: &wrapped_fields,
    };
    check_garde_peers(&garde_peer_refs, &peer_context, &at)?;
    // Add new fields from `add` parameter
    let add_garde_skip = any_garde.then(|| quote! { #[garde(skip)] });
    for (field_name, field_ty) in input.add.iter().flatten() {
        let field_ident = field_ident(field_name);
        // A bare `Vec` part reads "no parts" as `[]` (as a source one does).
        let vec_default = (input.multipart && crate::multipart_impl::is_vec_type(field_ty))
            .then(|| quote! { #[form_data(default)] });
        let field = quote! {
            #add_garde_skip
            #vec_default
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
    // (and the source container's own `#[garde(..)]`, e.g. `context`, its
    // paths re-rooted at the source module).
    let garde_derive = any_garde.then(|| quote! { ::garde::Validate, });
    let container_garde_attrs: Vec<syn::Attribute> = if any_garde {
        parsed_struct
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("garde"))
            .map(|attr| reroot_garde_attr(attr, &source_module_path))
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

    // The `from_model` load reads the source column behind each relation it
    // emits (R21-SM2-L1, round 21), even when `omit` removed that field from
    // the DTO: it must be readable from the DTO module too. A `relation_adapters`
    // relation is loaded the same way (only its value expression differs).
    for rel in &relation_fields {
        if let Some(column) = source_column_read(rel, &parsed_struct.fields)
            && let Some(field) = source_field_named(&parsed_struct.fields, &column)
        {
            all_readable &=
                assignments::readable_from(&field.vis, &source_module_path, call_site.as_deref());
        }
    }

    // Check if there are any relation fields
    let has_relation_fields = field_mappings
        .iter()
        .any(|mapping| mapping.relation.is_some());
    // The DTO can be built from a source value: no `add` fields, no
    // relations (absent from the source `Model`) and every source field
    // readable from the call site (a private field of another module is
    // E0616; the `From` is left out instead).
    let buildable = input.add.is_none() && !has_relation_fields && all_readable;

    // In multipart mode, skip From and from_model impls entirely
    let source_type: &syn::Type = &input.source_type;
    let (from_impl, from_model_impl) = if input.multipart {
        (quote! {}, quote! {})
    } else {
        let from_impl = if buildable {
            let field_assignments = assignments::source_field_assignments(&field_mappings, false);

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
            if is_source_seaorm_model && input.add.is_none() && has_relation_fields && all_readable
            {
                generate_from_model_with_relations(
                    new_type_name,
                    source_type,
                    &field_mappings,
                    &relation_fields,
                    &source_module_path,
                    &parsed_struct.fields,
                )?
            } else {
                quote! {}
            };

        (from_impl, from_model_impl)
    };

    let allow_deprecated = any_deprecated.then(|| quote! { #[allow(deprecated)] });

    // The container attributes the emitted struct carries for its serde /
    // Multipart derive — also the ones the registered metadata repeats.
    // Multipart mode emits `#[serde(rename_all = ...)]` so Multipart applies
    // the rename at runtime AND the Schema derive reads it via the
    // extract_rename_all() fallback for OpenAPI field naming (none when the
    // source has no rename_all: raw names, see `multipart_raw_names`). Both
    // modes keep a source container `#[serde(default)]` as a DTO-building fn.
    let multipart_rename_all_attr =
        (!multipart_raw_names).then(|| quote! { #[serde(rename_all = #effective_rename_all)] });
    let default_assignments =
        buildable.then(|| assignments::source_field_assignments(&field_mappings, true));
    let (container_attrs, container_default_fn) = if input.multipart {
        let carry = multipart_carry::container(
            &parsed_struct.attrs,
            &source_module_path,
            source_type,
            new_type_name,
            default_assignments.as_deref(),
        );
        let container_carry = carry.attrs;
        // The container default is not carried: the bare `Vec` parts it
        // would have filled default to `[]` themselves.
        if carry.helper.is_empty() {
            for (index, field, metadata) in container_vec_fields {
                field_tokens[index] = field;
                metadata_field_tokens[index] = metadata;
            }
        }
        (
            quote! {
                #multipart_rename_all_attr
                #container_carry
            },
            carry.helper,
        )
    } else {
        let carry = multipart_carry::container_default(
            &parsed_struct.attrs,
            &source_module_path,
            source_type,
            new_type_name,
            default_assignments.as_deref(),
        );
        let default_attr = carry.attrs;
        (
            quote! {
                #rename_all_attr
                #(#serde_attrs_without_rename_all)*
                #default_attr
            },
            carry.helper,
        )
    };

    // Generate the new struct (with inline types for circular relations first)
    let generated_tokens: proc_macro2::TokenStream = if input.multipart {
        // Multipart mode: derive Multipart instead of serde
        quote! {
            #(#inline_type_definitions)*

            #container_default_fn

            #(#struct_doc_attrs)*
            #allow_deprecated
            #[derive(vespera::Multipart, #garde_derive #clone_derive #schema_derive)]
            #(#container_garde_attrs)*
            #schema_name_attr
            #container_attrs
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

            #container_default_fn

            #(#struct_doc_attrs)*
            #allow_deprecated
            #[derive(serde::Serialize, serde::Deserialize, #garde_derive #clone_derive #schema_derive)]
            #(#container_garde_attrs)*
            #schema_name_attr
            #container_attrs
            pub struct #new_type_name {
                #(#field_tokens),*
            }

            #from_impl
            #from_model_impl
        }
    };

    // If custom name is provided, create metadata for direct registration
    // This ensures the schema appears in OpenAPI even when `ignore` is set.
    // The definition is the emitted struct without its derives: the docs
    // (schema description) and the same container attributes.
    let metadata = input.schema_name.as_ref().map(|custom_name| {
        let struct_def = quote! {
            #(#struct_doc_attrs)*
            #container_attrs
            pub struct #new_type_name {
                #(#metadata_field_tokens),*
            }
        };
        crate::schema_impl::named_type_metadata(custom_name, new_type_name, struct_def.to_string())
    });

    Ok((generated_tokens, metadata))
}

pub(super) mod assignments;

/// Shared with `from_model::target_shape`, which predicts the emitted shape.
pub(in crate::schema_macro) use assignments::readable_from;
mod checks;
mod multipart_carry;
mod resolve;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_field_attrs;
#[cfg(test)]
mod tests_refs;
#[cfg(test)]
mod tests_round13;
#[cfg(test)]
mod tests_round18;
#[cfg(test)]
mod tests_round21;
