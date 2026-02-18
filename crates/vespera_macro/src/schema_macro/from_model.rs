//! `from_model` implementation generation
//!
//! Generates async `from_model` implementations for `SeaORM` models with relations.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::{
    circular::{
        analyze_circular_refs, generate_inline_struct_construction,
        generate_inline_type_construction,
    },
    file_lookup::{find_fk_column_from_target_entity, find_struct_from_schema_path},
    seaorm::RelationFieldInfo,
    type_utils::snake_to_pascal_case,
};
use crate::metadata::StructMetadata;

/// Build Entity path from Schema path.
/// e.g., `crate::models::user::Schema` -> `crate::models::user::Entity`
#[allow(clippy::too_many_lines, clippy::option_if_let_else)]
pub fn build_entity_path_from_schema_path(
    schema_path: &TokenStream,
    _source_module_path: &[String],
) -> TokenStream {
    // Parse the schema path to extract segments
    let path_str = schema_path.to_string();
    let segments: Vec<&str> = path_str.split("::").map(str::trim).collect();

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
                    // HasMany with relation_enum: use FK-based query on target entity
                    // HasMany without relation_enum: use standard find_related
                    if let Some(ref via_rel_value) = rel.via_rel {
                        // Look up the FK column from the target entity
                        let schema_path_str = rel.schema_path.to_string().replace(' ', "");
                        if let Some(fk_col_name) = find_fk_column_from_target_entity(&schema_path_str, via_rel_value) {
                            // Convert snake_case FK column to PascalCase for Column enum
                            let fk_col_pascal = snake_to_pascal_case(&fk_col_name);
                            let fk_col_ident = syn::Ident::new(&fk_col_pascal, proc_macro2::Span::call_site());

                            // Build the Column path: entity_path without ::Entity, then ::Column::FkCol
                            // e.g., crate::models::notification::Entity -> crate::models::notification::Column::TargetUserId
                            let entity_path_str = entity_path.to_string().replace(' ', "");
                            let column_path_str = entity_path_str.replace(":: Entity", ":: Column");
                            let column_path_idents: Vec<syn::Ident> = column_path_str.split("::").map(str::trim).filter(|s| !s.is_empty()).map(|s| syn::Ident::new(s, proc_macro2::Span::call_site())).collect();

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
                            // FK column not found - fall back to empty vec with warning comment
                            quote! {
                                // WARNING: Could not find FK column for relation_enum, using empty vec
                                let #field_name: Vec<_> = vec![];
                            }
                        }
                    } else if let Some(via_rel_value) = &rel.relation_enum {
                        // Has relation_enum but no via_rel - try using relation_enum as via_rel
                        let schema_path_str = rel.schema_path.to_string().replace(' ', "");
                        if let Some(fk_col_name) = find_fk_column_from_target_entity(&schema_path_str, via_rel_value) {
                            let fk_col_pascal = snake_to_pascal_case(&fk_col_name);
                            let fk_col_ident = syn::Ident::new(&fk_col_pascal, proc_macro2::Span::call_site());

                            let entity_path_str = entity_path.to_string().replace(' ', "");
                            let column_path_str = entity_path_str.replace(":: Entity", ":: Column");
                            let column_path_idents: Vec<syn::Ident> = column_path_str.split("::").map(str::trim).filter(|s| !s.is_empty()).map(|s| syn::Ident::new(s, proc_macro2::Span::call_site())).collect();

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
                            // FK column not found - fall back to empty vec
                            quote! {
                                // WARNING: Could not find FK column for relation_enum, using empty vec
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
            let analysis = analyze_circular_refs(source_module_path, &model.definition);
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
                    let related_def_str = related_model_from_file.as_ref().map_or("", |s| s.definition.as_str());

                    // Analyze circular references, FK relations, and FK optionality in ONE pass
                    let analysis = analyze_circular_refs(source_module_path, related_def_str);
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
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    fn test_build_entity_path_from_schema_path() {
        let schema_path = quote! { crate::models::user::Schema };
        let result = build_entity_path_from_schema_path(&schema_path, &[]);
        let output = result.to_string();
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("user"));
        assert!(output.contains("Entity"));
        assert!(!output.contains("Schema"));
    }

    #[test]
    fn test_build_entity_path_simple() {
        let schema_path = quote! { user::Schema };
        let result = build_entity_path_from_schema_path(&schema_path, &[]);
        let output = result.to_string();
        assert!(output.contains("user"));
        assert!(output.contains("Entity"));
    }

    #[test]
    fn test_build_entity_path_deeply_nested() {
        let schema_path = quote! { crate::api::models::entities::user::Schema };
        let result = build_entity_path_from_schema_path(&schema_path, &[]);
        let output = result.to_string();
        assert!(output.contains("api"));
        assert!(output.contains("models"));
        assert!(output.contains("entities"));
        assert!(output.contains("user"));
        assert!(output.contains("Entity"));
        assert!(!output.contains("Schema"));
    }

    #[test]
    fn test_build_entity_path_single_segment() {
        let schema_path = quote! { Schema };
        let result = build_entity_path_from_schema_path(&schema_path, &[]);
        let output = result.to_string();
        assert!(output.contains("Entity"));
    }

    // Tests for generate_from_model_with_relations

    fn create_test_relation_info(
        field_name: &str,
        relation_type: &str,
        schema_path: TokenStream,
        is_optional: bool,
    ) -> RelationFieldInfo {
        RelationFieldInfo {
            field_name: syn::Ident::new(field_name, proc_macro2::Span::call_site()),
            relation_type: relation_type.to_string(),
            schema_path,
            is_optional,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        }
    }

    #[test]
    fn test_generate_from_model_with_required_relation() {
        let new_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        // Required relation (is_optional = false)
        let relation_fields = vec![create_test_relation_info(
            "user",
            "HasOne",
            quote! { user::Schema },
            false,
        )];
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl MemoSchema"));
        // Required relations should have RecordNotFound error handling
        assert!(output.contains("DbErr :: RecordNotFound"));
    }

    #[test]
    fn test_generate_from_model_with_wrapped_fields() {
        let new_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        // Field with wrapped=true means it needs Some() wrapping
        let field_mappings = vec![(
            syn::Ident::new("id", proc_macro2::Span::call_site()),
            syn::Ident::new("id", proc_macro2::Span::call_site()),
            true, // wrapped
            false,
        )];
        let relation_fields = vec![];
        let source_module_path = vec!["crate".to_string()];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("Some (model . id)"));
    }

    #[test]
    fn test_generate_from_model_with_has_one_optional() {
        let new_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        let relation_fields = vec![create_test_relation_info(
            "user",
            "HasOne",
            quote! { user::Schema },
            true,
        )];
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl MemoSchema"));
        assert!(output.contains("pub async fn from_model"));
        // quote! produces spaced output like "sea_orm :: DatabaseConnection"
        assert!(output.contains("sea_orm :: DatabaseConnection"));
        assert!(output.contains("Result < Self , sea_orm :: DbErr >"));
        assert!(output.contains("find_related"));
        assert!(output.contains(". one (db)"));
    }

    #[test]
    fn test_generate_from_model_with_has_many() {
        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        let relation_fields = vec![create_test_relation_info(
            "memos",
            "HasMany",
            quote! { memo::Schema },
            false,
        )];
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl UserSchema"));
        assert!(output.contains("pub async fn from_model"));
        assert!(output.contains(". all (db)"));
    }

    #[test]
    fn test_generate_from_model_with_belongs_to() {
        let new_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        let relation_fields = vec![create_test_relation_info(
            "user",
            "BelongsTo",
            quote! { user::Schema },
            true,
        )];
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl MemoSchema"));
        assert!(output.contains("find_related"));
        assert!(output.contains(". one (db)"));
    }

    #[test]
    fn test_generate_from_model_no_relations() {
        let new_type_name = syn::Ident::new("SimpleSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("name", proc_macro2::Span::call_site()),
                syn::Ident::new("name", proc_macro2::Span::call_site()),
                false,
                false,
            ),
        ];
        let relation_fields = vec![];
        let source_module_path = vec!["crate".to_string()];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl SimpleSchema"));
        assert!(output.contains("id : model . id"));
        assert!(output.contains("name : model . name"));
    }

    #[test]
    fn test_generate_from_model_with_inline_type() {
        let new_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        // Relation with inline type info (for circular references)
        let mut rel_info =
            create_test_relation_info("user", "HasOne", quote! { user::Schema }, true);
        rel_info.inline_type_info = Some((
            syn::Ident::new("MemoSchema_User", proc_macro2::Span::call_site()),
            vec!["id".to_string(), "name".to_string()],
        ));
        let relation_fields = vec![rel_info];
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl MemoSchema"));
        assert!(output.contains("find_related"));
    }

    #[test]
    fn test_generate_from_model_unknown_relation_type() {
        let new_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("unknown", proc_macro2::Span::call_site()),
                syn::Ident::new("unknown", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        // Unknown relation type
        let relation_fields = vec![create_test_relation_info(
            "unknown",
            "UnknownType",
            quote! { some::Schema },
            true,
        )];
        let source_module_path = vec!["crate".to_string()];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        // Unknown relation type should generate empty token (no load statement)
        assert!(output.contains("impl TestSchema"));
    }

    #[test]
    fn test_generate_from_model_relation_field_not_in_mappings() {
        let new_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            // Relation field with different source_ident
            (
                syn::Ident::new("owner", proc_macro2::Span::call_site()),
                syn::Ident::new("different_name", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        let relation_fields = vec![create_test_relation_info(
            "user",
            "HasOne",
            quote! { user::Schema },
            true,
        )];
        let source_module_path = vec!["crate".to_string()];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        // Should still generate valid code
        assert!(output.contains("impl TestSchema"));
    }

    #[test]
    fn test_generate_from_model_with_has_many_inline() {
        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];
        // HasMany with inline type
        let mut rel_info =
            create_test_relation_info("memos", "HasMany", quote! { memo::Schema }, false);
        rel_info.inline_type_info = Some((
            syn::Ident::new("UserSchema_Memos", proc_macro2::Span::call_site()),
            vec!["id".to_string(), "title".to_string()],
        ));
        let relation_fields = vec![rel_info];
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl UserSchema"));
        assert!(output.contains(". all (db)"));
        assert!(output.contains("into_iter"));
        assert!(output.contains("collect"));
    }

    // ============================================================
    // Coverage tests for file-based lookup branches
    // ============================================================

    #[test]
    #[serial]
    fn test_generate_from_model_needs_parent_stub_with_required_circular() {
        // Tests for from_model generation
        // Tests: HasMany relation where target model has REQUIRED circular back-ref
        // This triggers needs_parent_stub = true and generates parent stub fields
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create memo.rs with Model that has REQUIRED circular back-ref to user
        // The memo has `user: Box<UserSchema>` (not Option) - required
        let memo_model = r#"
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "user_id")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

        // Create user.rs
        let user_model = r"
pub struct Model {
    pub id: i32,
    pub name: String,
}
";
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        // Field mappings: id (regular), name (regular), memos (relation, HasMany)
        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("name", proc_macro2::Span::call_site()),
                syn::Ident::new("name", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                false,
                true, // is_relation
            ),
        ];

        // HasMany WITHOUT inline_type_info (triggers parent stub path)
        let relation_fields = vec![create_test_relation_info(
            "memos",
            "HasMany",
            quote! { crate::models::memo::Schema },
            false,
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        assert!(output.contains("from_model"));
        // Should have parent stub with __parent_stub__
        assert!(
            output.contains("__parent_stub__"),
            "Should have parent stub: {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_circular_has_one_optional() {
        // Tests for field name resolution
        // Tests: HasOne with circular reference, optional
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create profile.rs with circular back-ref to user
        let profile_model = r"
pub struct Model {
    pub id: i32,
    pub bio: String,
    pub user: BelongsTo<super::user::Entity>,
}
";
        std::fs::write(models_dir.join("profile.rs"), profile_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("profile", proc_macro2::Span::call_site()),
                syn::Ident::new("profile", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasOne, optional, WITHOUT inline_type_info
        let relation_fields = vec![create_test_relation_info(
            "profile",
            "HasOne",
            quote! { crate::models::profile::Schema },
            true, // optional
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Circular optional should have .map(|r| Box::new(...))
        assert!(
            output.contains(". map (| r |"),
            "Should have map for optional: {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_circular_has_one_required() {
        // Tests for relation conversion failure
        // Tests: HasOne with circular reference, required
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create profile.rs with circular back-ref to user
        let profile_model = r"
pub struct Model {
    pub id: i32,
    pub bio: String,
    pub user: BelongsTo<super::user::Entity>,
}
";
        std::fs::write(models_dir.join("profile.rs"), profile_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("profile", proc_macro2::Span::call_site()),
                syn::Ident::new("profile", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasOne, REQUIRED, WITHOUT inline_type_info
        let relation_fields = vec![create_test_relation_info(
            "profile",
            "HasOne",
            quote! { crate::models::profile::Schema },
            false, // required
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Required circular should have Box::new with error handling
        assert!(
            output.contains("Box :: new"),
            "Should have Box::new for required: {output}"
        );
        assert!(
            output.contains("ok_or_else"),
            "Should have ok_or_else: {output}"
        );
    }

    #[test]
    fn test_generate_from_model_unknown_relation_with_inline_type() {
        // Tests for unknown relation type handling
        // Tests: Unknown relation type WITH inline_type_info -> Default::default()
        let new_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("weird", proc_macro2::Span::call_site()),
                syn::Ident::new("weird", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // Unknown relation type WITH inline_type_info
        let mut rel_info = create_test_relation_info(
            "weird",
            "UnknownRelationType",
            quote! { some::Schema },
            true,
        );
        rel_info.inline_type_info = Some((
            syn::Ident::new("TestSchema_Weird", proc_macro2::Span::call_site()),
            vec!["id".to_string()],
        ));

        let relation_fields = vec![rel_info];
        let source_module_path = vec!["crate".to_string()];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        let output = tokens.to_string();
        assert!(output.contains("impl TestSchema"));
        // Unknown relation with inline type should use Default::default()
        assert!(
            output.contains("Default :: default ()"),
            "Should have Default::default(): {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_non_circular_has_one_with_fk_optional() {
        // Tests for field rename handling
        // Tests: HasOne with FK relations in target, no circular, optional
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create address.rs with FK relations but NO circular back-ref to user
        let address_model = r"
pub struct Model {
    pub id: i32,
    pub street: String,
    pub city_id: i32,
    pub city: BelongsTo<super::city::Entity>,
}
";
        std::fs::write(models_dir.join("address.rs"), address_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("address", proc_macro2::Span::call_site()),
                syn::Ident::new("address", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasOne, optional, no inline_type_info
        let relation_fields = vec![create_test_relation_info(
            "address",
            "HasOne",
            quote! { crate::models::address::Schema },
            true, // optional
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Non-circular with FK, optional should have match statement with async from_model
        assert!(
            output.contains("from_model (r , db) . await"),
            "Should have async from_model: {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_non_circular_has_one_with_fk_required() {
        // Tests for parent stub generation
        // Tests: HasOne with FK relations in target, no circular, required
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create address.rs with FK relations but NO circular back-ref to user
        let address_model = r"
pub struct Model {
    pub id: i32,
    pub street: String,
    pub city_id: i32,
    pub city: BelongsTo<super::city::Entity>,
}
";
        std::fs::write(models_dir.join("address.rs"), address_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("address", proc_macro2::Span::call_site()),
                syn::Ident::new("address", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasOne, REQUIRED, no inline_type_info
        let relation_fields = vec![create_test_relation_info(
            "address",
            "HasOne",
            quote! { crate::models::address::Schema },
            false, // required
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Required with FK should have Box::new with from_model call
        assert!(
            output.contains("Box :: new"),
            "Should have Box::new: {output}"
        );
        assert!(
            output.contains("from_model"),
            "Should have from_model: {output}"
        );
        assert!(
            output.contains("ok_or_else"),
            "Should have ok_or_else: {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_has_many_with_circular() {
        // Tests for quote generation
        // Tests: HasMany with circular reference
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create memo.rs with circular back-ref to user
        let memo_model = r"
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user: BelongsTo<super::user::Entity>,
}
";
        std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasMany WITHOUT inline_type_info - will use generate_inline_struct_construction
        let relation_fields = vec![create_test_relation_info(
            "memos",
            "HasMany",
            quote! { crate::models::memo::Schema },
            false,
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // HasMany with circular should have into_iter().map().collect()
        assert!(
            output.contains("into_iter ()"),
            "Should have into_iter: {output}"
        );
        assert!(output.contains(". map (| r |"), "Should have map: {output}");
        assert!(output.contains("collect"), "Should have collect: {output}");
    }

    #[test]
    #[serial]
    fn test_generate_from_model_has_many_with_fk_no_circular() {
        // Tests for multi-variant case handling
        // Tests: HasMany with FK relations in target, no circular
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create tag.rs with FK relations but NO circular back-ref to user
        let tag_model = r"
pub struct Model {
    pub id: i32,
    pub name: String,
    pub category_id: i32,
    pub category: BelongsTo<super::category::Entity>,
}
";
        std::fs::write(models_dir.join("tag.rs"), tag_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("tags", proc_macro2::Span::call_site()),
                syn::Ident::new("tags", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasMany, no inline_type_info
        let relation_fields = vec![create_test_relation_info(
            "tags",
            "HasMany",
            quote! { crate::models::tag::Schema },
            false,
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // HasMany with FK but no circular should use inline_struct_construction
        assert!(
            output.contains("into_iter ()"),
            "Should have into_iter: {output}"
        );
        assert!(output.contains(". map (| r |"), "Should have map: {output}");
        assert!(output.contains("collect"), "Should have collect: {output}");
    }

    #[test]
    #[serial]
    fn test_generate_from_model_inline_type_required() {
        // Tests: inline_type_info with required BelongsTo
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        let user_model = r"
pub struct Model {
    pub id: i32,
    pub name: String,
}
";
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::memo::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                syn::Ident::new("user", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // BelongsTo with inline_type_info, REQUIRED
        let mut rel_info = create_test_relation_info(
            "user",
            "BelongsTo",
            quote! { crate::models::user::Schema },
            false, // required
        );
        rel_info.inline_type_info = Some((
            syn::Ident::new("MemoSchema_User", proc_macro2::Span::call_site()),
            vec!["id".to_string(), "name".to_string()],
        ));

        let relation_fields = vec![rel_info];
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl MemoSchema"));
        // Required inline type should have Box::new with ok_or_else
        assert!(
            output.contains("Box :: new"),
            "Should have Box::new: {output}"
        );
        assert!(
            output.contains("ok_or_else"),
            "Should have ok_or_else: {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_parent_stub_all_relation_types() {
        // Tests for relation type variants
        // Tests: Parent stub generation with:
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create memo.rs with REQUIRED circular back-ref to user
        // This triggers needs_parent_stub = true
        let memo_model = r#"
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "user_id")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

        // Create profile.rs (for optional single relation)
        let profile_model = r"
pub struct Model {
    pub id: i32,
    pub bio: String,
}
";
        std::fs::write(models_dir.join("profile.rs"), profile_model).unwrap();

        // Create settings.rs (for required single relation)
        let settings_model = r"
pub struct Model {
    pub id: i32,
    pub theme: String,
}
";
        std::fs::write(models_dir.join("settings.rs"), settings_model).unwrap();

        // Save and set CARGO_MANIFEST_DIR
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        // Field mappings with various relation types
        let field_mappings = vec![
            // Regular field
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            // HasMany - this one triggers needs_parent_stub
            (
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                syn::Ident::new("memos", proc_macro2::Span::call_site()),
                false,
                true,
            ),
            // Optional single relation
            (
                syn::Ident::new("profile", proc_macro2::Span::call_site()),
                syn::Ident::new("profile", proc_macro2::Span::call_site()),
                false,
                true,
            ),
            // Required single relation
            (
                syn::Ident::new("settings", proc_macro2::Span::call_site()),
                syn::Ident::new("settings", proc_macro2::Span::call_site()),
                false,
                true,
            ),
            // Relation field NOT in relation_fields
            (
                syn::Ident::new("orphan_rel", proc_macro2::Span::call_site()),
                syn::Ident::new("orphan_rel", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // Relation fields - note: orphan_rel is NOT included here
        let relation_fields = vec![
            // HasMany without inline_type_info (triggers needs_parent_stub)
            create_test_relation_info(
                "memos",
                "HasMany",
                quote! { crate::models::memo::Schema },
                false,
            ),
            // Optional HasOne
            create_test_relation_info(
                "profile",
                "HasOne",
                quote! { crate::models::profile::Schema },
                true, // optional
            ),
            // Required BelongsTo
            create_test_relation_info(
                "settings",
                "BelongsTo",
                quote! { crate::models::settings::Schema },
                false, // required
            ),
            // Note: orphan_rel is NOT in relation_fields
        ];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];

        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        // Restore CARGO_MANIFEST_DIR
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Should have parent stub
        assert!(
            output.contains("__parent_stub__"),
            "Should have parent stub: {output}"
        );
        // Parent stub should have various default values
        // Line 113: memos: vec![]
        assert!(
            output.contains("memos : vec ! []"),
            "Should have memos: vec![]: {output}"
        );
        // Line 114 & 117: profile/settings: None (both optional and required single relations)
        // (Both produce None in parent stub)
        assert!(
            output.contains("profile : None") || output.contains("settings : None"),
            "Should have None for single relations: {output}"
        );
        // Line 120: orphan_rel: Default::default()
        assert!(
            output.contains("Default :: default ()"),
            "Should have Default::default() for orphan: {output}"
        );
    }

    // ============================================================
    // Tests for relation_enum + fk_column branches
    // ============================================================

    fn create_test_relation_info_full(
        field_name: &str,
        relation_type: &str,
        schema_path: TokenStream,
        is_optional: bool,
        relation_enum: Option<String>,
        fk_column: Option<String>,
        via_rel: Option<String>,
    ) -> RelationFieldInfo {
        RelationFieldInfo {
            field_name: syn::Ident::new(field_name, proc_macro2::Span::call_site()),
            relation_type: relation_type.to_string(),
            schema_path,
            is_optional,
            inline_type_info: None,
            relation_enum,
            fk_column,
            via_rel,
        }
    }

    #[test]
    fn test_generate_from_model_has_one_with_relation_enum_optional_with_fk() {
        // Tests for field name comparison
        // Tests: HasOne with relation_enum + optional + fk_column present
        let new_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("target_user", proc_macro2::Span::call_site()),
                syn::Ident::new("target_user", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasOne with relation_enum, optional, WITH fk_column
        let relation_fields = vec![create_test_relation_info_full(
            "target_user",
            "HasOne",
            quote! { user::Schema },
            true,                               // optional
            Some("TargetUser".to_string()),     // relation_enum
            Some("target_user_id".to_string()), // fk_column
            None,                               // via_rel
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl MemoSchema"));
        // Should have match statement checking FK field
        assert!(
            output.contains("match & model . target_user_id"),
            "Should match on FK field: {output}"
        );
        assert!(
            output.contains("Some (fk_value)"),
            "Should have Some(fk_value) arm: {output}"
        );
        assert!(
            output.contains("find_by_id"),
            "Should use find_by_id: {output}"
        );
    }

    #[test]
    fn test_generate_from_model_has_one_with_relation_enum_optional_no_fk() {
        // Tests for None branch
        // Tests: HasOne with relation_enum + optional + NO fk_column (fallback)
        let new_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("author", proc_macro2::Span::call_site()),
                syn::Ident::new("author", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasOne with relation_enum, optional, WITHOUT fk_column
        let relation_fields = vec![create_test_relation_info_full(
            "author",
            "HasOne",
            quote! { user::Schema },
            true,                       // optional
            Some("Author".to_string()), // relation_enum
            None,                       // NO fk_column
            None,                       // via_rel
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl MemoSchema"));
        // Fallback: use Entity::find_related(Relation::Variant)
        assert!(
            output.contains("Entity :: find_related (Relation :: Author)"),
            "Should use find_related with Relation enum: {output}"
        );
    }

    #[test]
    fn test_generate_from_model_belongs_to_with_relation_enum_required_with_fk() {
        // Tests for required relation field
        // Tests: BelongsTo with relation_enum + required + fk_column present
        let new_type_name = syn::Ident::new("CommentSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("post", proc_macro2::Span::call_site()),
                syn::Ident::new("post", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // BelongsTo with relation_enum, required, WITH fk_column
        let relation_fields = vec![create_test_relation_info_full(
            "post",
            "BelongsTo",
            quote! { post::Schema },
            false,                       // required
            Some("Post".to_string()),    // relation_enum
            Some("post_id".to_string()), // fk_column
            None,                        // via_rel
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "comment".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl CommentSchema"));
        // Should directly query by FK value
        assert!(
            output.contains("find_by_id (model . post_id . clone ())"),
            "Should use find_by_id with FK: {output}"
        );
    }

    #[test]
    fn test_generate_from_model_belongs_to_with_relation_enum_required_no_fk() {
        // Tests for skip condition
        // Tests: BelongsTo with relation_enum + required + NO fk_column (fallback)
        let new_type_name = syn::Ident::new("CommentSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("author", proc_macro2::Span::call_site()),
                syn::Ident::new("author", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // BelongsTo with relation_enum, required, WITHOUT fk_column
        let relation_fields = vec![create_test_relation_info_full(
            "author",
            "BelongsTo",
            quote! { user::Schema },
            false,                      // required
            Some("Author".to_string()), // relation_enum
            None,                       // NO fk_column
            None,                       // via_rel
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "comment".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );
        let output = tokens.to_string();

        assert!(output.contains("impl CommentSchema"));
        // Fallback: use Entity::find_related(Relation::Variant)
        assert!(
            output.contains("Entity :: find_related (Relation :: Author)"),
            "Should use find_related with Relation enum: {output}"
        );
    }

    // ============================================================
    // Tests for HasMany with via_rel/relation_enum
    // ============================================================

    #[test]
    #[serial]
    fn test_generate_from_model_has_many_with_via_rel_fk_found() {
        // Tests for HasMany with via_rel + FK column found
        // Tests: HasMany with via_rel + FK column found in target entity
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create notification.rs with matching relation_enum
        let notification_model = r#"
pub struct Model {
    pub id: i32,
    pub message: String,
    pub target_user_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "target_user_id", to = "id", relation_enum = "TargetUser")]
    pub target_user: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("notification.rs"), notification_model).unwrap();

        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("target_user_notifications", proc_macro2::Span::call_site()),
                syn::Ident::new("target_user_notifications", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasMany with via_rel
        let relation_fields = vec![create_test_relation_info_full(
            "target_user_notifications",
            "HasMany",
            quote! { crate::models::notification::Schema },
            false,
            None,
            None,
            Some("TargetUser".to_string()), // via_rel
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Should generate FK-based query
        assert!(
            output.contains("TargetUserId"),
            "Should have FK column identifier: {output}"
        );
        assert!(
            output.contains("into_column ()"),
            "Should have into_column: {output}"
        );
        assert!(
            output.contains("eq (model . id . clone ())"),
            "Should compare with model.id: {output}"
        );
        assert!(
            output.contains(". all (db)"),
            "Should use .all(db): {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_has_many_with_via_rel_fk_not_found() {
        // Tests for HasMany via_rel not found
        // Tests: HasMany with via_rel but FK column NOT found in target entity
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create notification.rs WITHOUT matching relation_enum
        let notification_model = r"
pub struct Model {
    pub id: i32,
    pub message: String,
}
";
        std::fs::write(models_dir.join("notification.rs"), notification_model).unwrap();

        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("notifications", proc_macro2::Span::call_site()),
                syn::Ident::new("notifications", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasMany with via_rel that won't find FK
        let relation_fields = vec![create_test_relation_info_full(
            "notifications",
            "HasMany",
            quote! { crate::models::notification::Schema },
            false,
            None,
            None,
            Some("NonExistentRelation".to_string()), // via_rel that won't match
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Should fall back to empty vec (WARNING comment won't appear in TokenStream)
        assert!(
            output.contains("vec ! []"),
            "Should fall back to empty vec: {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_has_many_with_relation_enum_fk_found() {
        // Tests for via_rel field matching
        // Tests: HasMany with relation_enum (no via_rel) + FK column found
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create comment.rs with matching relation_enum
        let comment_model = r#"
pub struct Model {
    pub id: i32,
    pub content: String,
    pub author_id: i32,
    #[sea_orm(belongs_to = "super::user::Entity", from = "author_id", to = "id", relation_enum = "AuthorComments")]
    pub author: BelongsTo<super::user::Entity>,
}
"#;
        std::fs::write(models_dir.join("comment.rs"), comment_model).unwrap();

        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("author_comments", proc_macro2::Span::call_site()),
                syn::Ident::new("author_comments", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasMany with relation_enum (no via_rel)
        let relation_fields = vec![create_test_relation_info_full(
            "author_comments",
            "HasMany",
            quote! { crate::models::comment::Schema },
            false,
            Some("AuthorComments".to_string()), // relation_enum
            None,
            None, // NO via_rel - will use relation_enum as via_rel
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Should generate FK-based query using relation_enum as via_rel
        assert!(
            output.contains("AuthorId"),
            "Should have FK column identifier: {output}"
        );
        assert!(
            output.contains("into_column ()"),
            "Should have into_column: {output}"
        );
        assert!(
            output.contains(". all (db)"),
            "Should use .all(db): {output}"
        );
    }

    #[test]
    #[serial]
    fn test_generate_from_model_has_many_with_relation_enum_fk_not_found() {
        // Tests for HasMany via_rel generation
        // Tests: HasMany with relation_enum (no via_rel) + FK column NOT found
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create post.rs WITHOUT matching relation_enum
        let post_model = r"
pub struct Model {
    pub id: i32,
    pub title: String,
}
";
        std::fs::write(models_dir.join("post.rs"), post_model).unwrap();

        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let new_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let source_type: Type = syn::parse_str("crate::models::user::Model").unwrap();

        let field_mappings = vec![
            (
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                syn::Ident::new("id", proc_macro2::Span::call_site()),
                false,
                false,
            ),
            (
                syn::Ident::new("authored_posts", proc_macro2::Span::call_site()),
                syn::Ident::new("authored_posts", proc_macro2::Span::call_site()),
                false,
                true,
            ),
        ];

        // HasMany with relation_enum that won't match (no via_rel)
        let relation_fields = vec![create_test_relation_info_full(
            "authored_posts",
            "HasMany",
            quote! { crate::models::post::Schema },
            false,
            Some("NonExistentRelation".to_string()), // relation_enum that won't match
            None,
            None, // NO via_rel
        )];

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let tokens = generate_from_model_with_relations(
            &new_type_name,
            &source_type,
            &field_mappings,
            &relation_fields,
            &source_module_path,
            &HashMap::new(),
        );

        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        let output = tokens.to_string();
        assert!(output.contains("impl UserSchema"));
        // Should fall back to empty vec (WARNING comment won't appear in TokenStream)
        assert!(
            output.contains("vec ! []"),
            "Should fall back to empty vec: {output}"
        );
    }
}
