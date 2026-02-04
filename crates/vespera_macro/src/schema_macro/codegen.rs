//! Code generation utilities for schema macros
//!
//! Contains functions for:
//! - Converting Schema/SchemaRef to TokenStream
//! - Generating filtered schema code
//! - Generating From impls and from_model async methods

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;

use super::file_lookup::find_model_from_schema_path;
use super::relation::{
    RelationFieldInfo, build_entity_path_from_schema_path, detect_circular_fields,
    generate_inline_struct_construction, generate_inline_type_construction, has_fk_relations,
    is_circular_relation_required,
};
use super::type_utils::is_option_type;
use crate::metadata::StructMetadata;
use crate::parser::{
    extract_default, extract_field_rename, extract_rename_all, extract_skip,
    extract_skip_serializing_if, parse_type_to_schema_ref, rename_field, strip_raw_prefix,
};
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

/// Convert SchemaRef to TokenStream for code generation
pub fn schema_ref_to_tokens(schema_ref: &SchemaRef) -> TokenStream {
    match schema_ref {
        SchemaRef::Ref(reference) => {
            let ref_path = &reference.ref_path;
            quote! {
                vespera::schema::SchemaRef::Ref(vespera::schema::Reference::new(#ref_path.to_string()))
            }
        }
        SchemaRef::Inline(schema) => {
            let schema_tokens = schema_to_tokens(schema);
            quote! {
                vespera::schema::SchemaRef::Inline(Box::new(#schema_tokens))
            }
        }
    }
}

/// Convert Schema to TokenStream for code generation
pub fn schema_to_tokens(schema: &Schema) -> TokenStream {
    let schema_type_tokens = match &schema.schema_type {
        Some(SchemaType::String) => quote! { Some(vespera::schema::SchemaType::String) },
        Some(SchemaType::Number) => quote! { Some(vespera::schema::SchemaType::Number) },
        Some(SchemaType::Integer) => quote! { Some(vespera::schema::SchemaType::Integer) },
        Some(SchemaType::Boolean) => quote! { Some(vespera::schema::SchemaType::Boolean) },
        Some(SchemaType::Array) => quote! { Some(vespera::schema::SchemaType::Array) },
        Some(SchemaType::Object) => quote! { Some(vespera::schema::SchemaType::Object) },
        Some(SchemaType::Null) => quote! { Some(vespera::schema::SchemaType::Null) },
        None => quote! { None },
    };

    let format_tokens = match &schema.format {
        Some(f) => quote! { Some(#f.to_string()) },
        None => quote! { None },
    };

    let nullable_tokens = match schema.nullable {
        Some(true) => quote! { Some(true) },
        Some(false) => quote! { Some(false) },
        None => quote! { None },
    };

    let ref_path_tokens = match &schema.ref_path {
        Some(rp) => quote! { Some(#rp.to_string()) },
        None => quote! { None },
    };

    let items_tokens = match &schema.items {
        Some(items) => {
            let inner = schema_ref_to_tokens(items);
            quote! { Some(Box::new(#inner)) }
        }
        None => quote! { None },
    };

    let properties_tokens = match &schema.properties {
        Some(props) => {
            let entries: Vec<_> = props
                .iter()
                .map(|(k, v)| {
                    let v_tokens = schema_ref_to_tokens(v);
                    quote! { (#k.to_string(), #v_tokens) }
                })
                .collect();
            quote! {
                Some({
                    let mut map = std::collections::BTreeMap::new();
                    #(map.insert(#entries.0, #entries.1);)*
                    map
                })
            }
        }
        None => quote! { None },
    };

    let required_tokens = match &schema.required {
        Some(req) => {
            let req_strs: Vec<_> = req.iter().map(|s| s.as_str()).collect();
            quote! { Some(vec![#(#req_strs.to_string()),*]) }
        }
        None => quote! { None },
    };

    quote! {
        vespera::schema::Schema {
            ref_path: #ref_path_tokens,
            schema_type: #schema_type_tokens,
            format: #format_tokens,
            nullable: #nullable_tokens,
            items: #items_tokens,
            properties: #properties_tokens,
            required: #required_tokens,
            ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
        }
    }
}

/// Generate Schema construction code with field filtering
pub fn generate_filtered_schema(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
    schema_storage: &[StructMetadata],
) -> Result<TokenStream, syn::Error> {
    let rename_all = extract_rename_all(&struct_item.attrs);

    // Build known_schemas and struct_definitions for type resolution
    let known_schemas: std::collections::HashMap<String, String> = schema_storage
        .iter()
        .map(|s| (s.name.clone(), s.definition.clone()))
        .collect();
    let struct_definitions = known_schemas.clone();

    let mut property_tokens = Vec::new();
    let mut required_fields = Vec::new();

    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            // Skip if serde(skip)
            if extract_skip(&field.attrs) {
                continue;
            }

            let rust_field_name = field
                .ident
                .as_ref()
                .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                .unwrap_or_else(|| "unknown".to_string());

            // Apply rename
            let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                renamed
            } else {
                rename_field(&rust_field_name, rename_all.as_deref())
            };

            // Apply omit filter (check both rust name and json name)
            if !omit_set.is_empty()
                && (omit_set.contains(&rust_field_name) || omit_set.contains(&field_name))
            {
                continue;
            }

            // Apply pick filter (check both rust name and json name)
            if !pick_set.is_empty()
                && !pick_set.contains(&rust_field_name)
                && !pick_set.contains(&field_name)
            {
                continue;
            }

            let field_type = &field.ty;

            // Generate schema for field type
            let schema_ref =
                parse_type_to_schema_ref(field_type, &known_schemas, &struct_definitions);
            let schema_ref_tokens = schema_ref_to_tokens(&schema_ref);

            property_tokens.push(quote! {
                properties.insert(#field_name.to_string(), #schema_ref_tokens);
            });

            // Check if field is required (not Option, no default, no skip_serializing_if)
            let has_default = extract_default(&field.attrs).is_some();
            let has_skip_serializing_if = extract_skip_serializing_if(&field.attrs);
            let is_optional = is_option_type(field_type);

            if !is_optional && !has_default && !has_skip_serializing_if {
                required_fields.push(field_name.clone());
            }
        }
    }

    let required_tokens = if required_fields.is_empty() {
        quote! { None }
    } else {
        let required_strs: Vec<&str> = required_fields.iter().map(|s| s.as_str()).collect();
        quote! { Some(vec![#(#required_strs.to_string()),*]) }
    };

    Ok(quote! {
        {
            let mut properties = std::collections::BTreeMap::new();
            #(#property_tokens)*
            vespera::schema::Schema {
                schema_type: Some(vespera::schema::SchemaType::Object),
                properties: if properties.is_empty() { None } else { Some(properties) },
                required: #required_tokens,
                ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
            }
        }
    })
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
    source_type: &syn::Type,
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
        let related_model = find_model_from_schema_path(&model_path_str);

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
                    let related_model_from_file = find_model_from_schema_path(&model_path_str);

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
                                        // Target schema has FK relations → use async from_model()
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
                                        // Target schema has no FK relations → use sync From::from()
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
    use vespera_core::schema::{Reference, Schema, SchemaRef, SchemaType};

    #[test]
    fn test_schema_ref_to_tokens_ref_variant() {
        let schema_ref = SchemaRef::Ref(Reference::new("#/components/schemas/User".to_string()));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();

        assert!(output.contains("SchemaRef :: Ref"));
        assert!(output.contains("Reference :: new"));
    }

    #[test]
    fn test_schema_ref_to_tokens_inline_variant() {
        let schema = Schema::new(SchemaType::String);
        let schema_ref = SchemaRef::Inline(Box::new(schema));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();

        assert!(output.contains("SchemaRef :: Inline"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_schema_to_tokens_string_type() {
        let schema = Schema::new(SchemaType::String);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: String"));
    }

    #[test]
    fn test_schema_to_tokens_integer_type() {
        let schema = Schema::new(SchemaType::Integer);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Integer"));
    }

    #[test]
    fn test_schema_to_tokens_number_type() {
        let schema = Schema::new(SchemaType::Number);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Number"));
    }

    #[test]
    fn test_schema_to_tokens_boolean_type() {
        let schema = Schema::new(SchemaType::Boolean);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Boolean"));
    }

    #[test]
    fn test_schema_to_tokens_array_type() {
        let schema = Schema::new(SchemaType::Array);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Array"));
    }

    #[test]
    fn test_schema_to_tokens_object_type() {
        let schema = Schema::new(SchemaType::Object);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Object"));
    }

    #[test]
    fn test_schema_to_tokens_null_type() {
        let schema = Schema::new(SchemaType::Null);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Null"));
    }

    #[test]
    fn test_schema_to_tokens_with_format() {
        let mut schema = Schema::new(SchemaType::String);
        schema.format = Some("date-time".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("date-time"));
    }

    #[test]
    fn test_schema_to_tokens_with_nullable() {
        let mut schema = Schema::new(SchemaType::String);
        schema.nullable = Some(true);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("Some (true)"));
    }

    #[test]
    fn test_schema_to_tokens_with_ref_path() {
        let mut schema = Schema::new(SchemaType::Object);
        schema.ref_path = Some("#/components/schemas/User".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("#/components/schemas/User"));
    }

    #[test]
    fn test_schema_to_tokens_with_items() {
        let mut schema = Schema::new(SchemaType::Array);
        let item_schema = Schema::new(SchemaType::String);
        schema.items = Some(Box::new(SchemaRef::Inline(Box::new(item_schema))));
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("items"));
        assert!(output.contains("Some (Box :: new"));
    }

    #[test]
    fn test_schema_to_tokens_with_properties() {
        use std::collections::BTreeMap;

        let mut schema = Schema::new(SchemaType::Object);
        let mut props = BTreeMap::new();
        props.insert(
            "name".to_string(),
            SchemaRef::Inline(Box::new(Schema::new(SchemaType::String))),
        );
        schema.properties = Some(props);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("properties"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_schema_to_tokens_with_required() {
        let mut schema = Schema::new(SchemaType::Object);
        schema.required = Some(vec!["id".to_string(), "name".to_string()]);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("required"));
        assert!(output.contains("id"));
        assert!(output.contains("name"));
    }
}
