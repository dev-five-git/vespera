//! Async `from_model` impl generation for SeaORM models with
//! relations (circular handling, FK lookups, parent stubs).

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::Type;

use super::super::{
    circular::{generate_inline_struct_construction, generate_inline_type_construction},
    file_cache::{get_circular_analysis, get_fk_column, get_struct_from_schema_path},
    seaorm::RelationFieldInfo,
    type_utils::{normalize_token_str, snake_to_pascal_case},
};
use super::build_entity_path_from_schema_path;
use crate::metadata::StructMetadata;

/// Generate `from_model` impl for `SeaORM` Model WITH relations (async version).
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
#[allow(clippy::too_many_lines, clippy::option_if_let_else)]
pub fn generate_from_model_with_relations(
    new_type_name: &syn::Ident,
    source_type: &Type,
    field_mappings: &[(syn::Ident, syn::Ident, bool, bool)],
    relation_fields: &[RelationFieldInfo],
    source_module_path: &[String],
    _schema_storage: &HashMap<String, StructMetadata>,
) -> TokenStream {
    // Build relation loading statements
    let relation_loads: Vec<TokenStream> = relation_fields
        .iter()
        .map(|rel| {
            let field_name = &rel.field_name;
            let entity_path = build_entity_path_from_schema_path(&rel.schema_path, source_module_path);

            match rel.relation_type.as_str() {
                "HasOne" | "BelongsTo" => {
                    // When relation_enum is specified, use the specific Relation variant
                    // This handles cases where multiple relations point to the same Entity type
                    if let Some(ref relation_enum_name) = rel.relation_enum {
                        let relation_variant = syn::Ident::new(relation_enum_name, proc_macro2::Span::call_site());

                        if rel.is_optional {
                            // Optional FK: load only if FK value exists
                            if let Some(ref fk_col) = rel.fk_column {
                                let fk_ident = syn::Ident::new(fk_col, proc_macro2::Span::call_site());
                                quote! {
                                    let #field_name = match &model.#fk_ident {
                                        Some(fk_value) => #entity_path::find_by_id(fk_value.clone()).one(db).await?,
                                        None => None,
                                    };
                                }
                            } else {
                                // Fallback: use find_related with Relation enum
                                quote! {
                                    let #field_name = Entity::find_related(Relation::#relation_variant)
                                        .filter(<Entity as sea_orm::EntityTrait>::PrimaryKey::eq(&model))
                                        .one(db)
                                        .await?;
                                }
                            }
                        } else {
                            // Required FK: directly query by FK value
                            if let Some(ref fk_col) = rel.fk_column {
                                let fk_ident = syn::Ident::new(fk_col, proc_macro2::Span::call_site());
                                quote! {
                                    let #field_name = #entity_path::find_by_id(model.#fk_ident.clone()).one(db).await?;
                                }
                            } else {
                                // Fallback: use find_related with Relation enum
                                quote! {
                                    let #field_name = Entity::find_related(Relation::#relation_variant)
                                        .filter(<Entity as sea_orm::EntityTrait>::PrimaryKey::eq(&model))
                                        .one(db)
                                        .await?;
                                }
                            }
                        }
                    } else {
                        // Standard case: single relation to target entity, use find_related
                        quote! {
                            let #field_name = model.find_related(#entity_path).one(db).await?;
                        }
                    }
                }
                "HasMany" => {
                    // Try via_rel first, fall back to relation_enum as FK source
                    let fk_rel_source = rel.via_rel.as_ref().or(rel.relation_enum.as_ref());
                    if let Some(via_rel_value) = fk_rel_source {
                        let schema_path_str = normalize_token_str(&rel.schema_path);
                        if let Some(fk_col_name) = get_fk_column(&schema_path_str, via_rel_value) {
                            let fk_col_pascal = snake_to_pascal_case(&fk_col_name);
                            let fk_col_ident = syn::Ident::new(&fk_col_pascal, proc_macro2::Span::call_site());

                            let entity_path_str = normalize_token_str(&entity_path);
                            let column_path_str = entity_path_str.replace(":: Entity", ":: Column");
                            let column_path_idents: Vec<syn::Ident> = column_path_str
                                .split("::")
                                .filter_map(|s| {
                                    let trimmed = s.trim();
                                    if trimmed.is_empty() { None } else { Some(syn::Ident::new(trimmed, proc_macro2::Span::call_site())) }
                                })
                                .collect();

                            quote! {
                                let #field_name = #(#column_path_idents)::*::#fk_col_ident
                                    .into_column()
                                    .eq(model.id.clone())
                                    .into_condition();
                                let #field_name = #entity_path::find()
                                    .filter(#field_name)
                                    .all(db)
                                    .await?;
                            }
                        } else {
                            quote! {
                                // WARNING: Could not find FK column for relation, using empty vec
                                let #field_name: Vec<_> = vec![];
                            }
                        }
                    } else {
                        // Standard HasMany - use find_related
                        quote! {
                            let #field_name = model.find_related(#entity_path).all(db).await?;
                        }
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
        // A parent stub is needed whenever a relation's inline construction can
        // emit `__parent_stub__` for a REQUIRED circular back-reference. That
        // is NOT HasMany-only: a required circular HasOne/BelongsTo (with no
        // inline type) also routes through `generate_inline_struct_construction`
        // (see the `has_circular` arm below) and references the same stub.
        // Excluding them generated code referencing an undefined
        // `__parent_stub__` local for that schema shape.
        if !matches!(
            rel.relation_type.as_str(),
            "HasMany" | "HasOne" | "BelongsTo"
        ) {
            return false;
        }
        // If using inline type, circular fields are excluded, so no parent stub needed
        if rel.inline_type_info.is_some() {
            return false;
        }
        let schema_path_str = normalize_token_str(&rel.schema_path);
        let model_path_str = schema_path_str.replace("::Schema", "::Model");
        let related_model = get_struct_from_schema_path(&model_path_str);

        if let Some(ref model) = related_model {
            let analysis = get_circular_analysis(source_module_path, &model.definition);
            // Check if any circular field is a required relation
            analysis.circular_fields.iter().any(|cf| {
                analysis
                    .circular_field_required
                    .get(cf)
                    .copied()
                    .unwrap_or(false)
            })
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
                            _ => {
                                let message = format!(
                                    "schema_type! cannot generate a circular parent stub for required relation field `{}`; make the relation `Option<...>` to break the cycle",
                                    rel.field_name
                                );
                                let error = syn::Error::new(rel.field_name.span(), message)
                                    .to_compile_error();
                                quote_spanned! { rel.field_name.span() => #new_ident: { #error } }
                            }
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

    // Pre-build relation lookup for O(1) access in field assignments loop
    let relation_by_name: HashMap<&syn::Ident, &RelationFieldInfo> = relation_fields
        .iter()
        .map(|rel| (&rel.field_name, rel))
        .collect();

    // Build field assignments
    // For relation fields, check for circular references and use inline construction if needed
    let field_assignments: Vec<TokenStream> = field_mappings
        .iter()
        .map(|(new_ident, source_ident, wrapped, is_relation)| {
            if *is_relation {
                // Find the relation info for this field
                if let Some(rel) = relation_by_name.get(source_ident) {
                    let schema_path = &rel.schema_path;

                    // Try to find the related MODEL definition to check for circular refs
                    // The schema_path is like "crate::models::user::Schema", but the actual
                    // struct is "Model" in the same module. We need to look up the Model
                    // to see if it has relations pointing back to us.
                    let schema_path_str = normalize_token_str(schema_path);

                    // Convert schema path to model path: Schema -> Model
                    let model_path_str = schema_path_str.replace("::Schema", "::Model");

                    // Try to find the related Model definition from file
                    let related_model_from_file = get_struct_from_schema_path(&model_path_str);

                    // Get the definition string
                    let related_def_str = related_model_from_file.as_ref().map_or("", |s| s.definition.as_str());

                    // Analyze circular references, FK relations, and FK optionality in ONE pass
                    let analysis = get_circular_analysis(source_module_path, related_def_str);
                    let circular_fields = &analysis.circular_fields;
                    let has_circular = !circular_fields.is_empty();

                    // Check if we have inline type info - if so, use the inline type
                    // instead of the original schema path
                    if let Some((ref inline_type_name, ref included_fields)) = rel.inline_type_info {
                        // Use inline type construction
                        let inline_construct = generate_inline_type_construction(inline_type_name, included_fields, related_def_str, "r");

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
                                    let inline_construct = generate_inline_struct_construction(schema_path, related_def_str, circular_fields, "r");
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
                                    // No circular ref - use has_fk_relations from the analysis
                                    let target_has_fk = analysis.has_fk_relations;

                                    if target_has_fk {
                                        // Target schema has FK relations -> use async from_model()
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
                                        // Target schema has no FK relations -> use sync From::from()
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
                                    let inline_construct = generate_inline_struct_construction(schema_path, related_def_str, circular_fields, "r");
                                    quote! {
                                        #new_ident: #source_ident.into_iter().map(|r| #inline_construct).collect()
                                    }
                                } else {
                                    // No circular ref - use has_fk_relations from the analysis
                                    let target_has_fk = analysis.has_fk_relations;

                                    if target_has_fk {
                                        // Target has FK relations but HasMany doesn't load nested data anyway,
                                        // so we use inline construction (flat fields only)
                                        let inline_construct = generate_inline_struct_construction(
                                            schema_path,
                                            related_def_str,
                                            &[], // no circular fields to exclude
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

#[cfg(test)]
mod tests;
