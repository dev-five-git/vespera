//! from_model implementation generation
//!
//! Generates async `from_model` implementations for SeaORM models with relations.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::circular::{
    detect_circular_fields, generate_inline_struct_construction, generate_inline_type_construction,
    has_fk_relations, is_circular_relation_required,
};
use super::file_lookup::find_struct_from_schema_path;
use super::seaorm::RelationFieldInfo;
use crate::metadata::StructMetadata;

/// Build Entity path from Schema path.
/// e.g., `crate::models::user::Schema` -> `crate::models::user::Entity`
pub fn build_entity_path_from_schema_path(
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
pub fn generate_from_model_with_relations(
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
                    let related_def_str = related_model_from_file.as_ref().map(|s| s.definition.as_str()).unwrap_or("");

                    // Check for circular references
                    // The source module path tells us what module we're in (e.g., ["crate", "models", "memo"])
                    // We need to check if the related model has any relation fields pointing back to our module
                    let circular_fields = detect_circular_fields(new_type_name.to_string().as_str(), source_module_path, related_def_str);

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
                                    let inline_construct = generate_inline_struct_construction(schema_path, related_def_str, &circular_fields, "r");
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
                                    let inline_construct = generate_inline_struct_construction(schema_path, related_def_str, &circular_fields, "r");
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

#[cfg(test)]
mod tests {
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
        );
        let output = tokens.to_string();

        assert!(output.contains("impl UserSchema"));
        assert!(output.contains(". all (db)"));
        assert!(output.contains("into_iter"));
        assert!(output.contains("collect"));
    }
}
