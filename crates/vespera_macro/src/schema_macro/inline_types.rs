//! Inline type generation for circular references
//!
//! When schemas have circular references, we generate inline types that
//! exclude the circular fields to prevent infinite recursion.

use proc_macro2::TokenStream;
use quote::quote;

use super::{
    circular::detect_circular_fields,
    file_lookup::find_model_from_schema_path,
    seaorm::{RelationFieldInfo, convert_type_with_chrono},
    type_utils::{
        extract_module_path_from_schema_path, is_seaorm_relation_type, snake_to_pascal_case,
    },
};
use crate::parser::{extract_rename_all, extract_skip};

/// Information about an inline relation type to generate
pub struct InlineRelationType {
    /// Name of the inline type (e.g., MemoResponseRel_User)
    pub type_name: syn::Ident,
    /// Fields to include (excluding circular references)
    pub fields: Vec<InlineField>,
    /// The effective rename_all strategy
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
/// The `schema_name_override` parameter allows using a custom schema name (e.g., "MemoSchema")
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
    // Parse the model struct
    let parsed_model: syn::ItemStruct = syn::parse_str(model_def).ok()?;

    // IMPORTANT: Use the TARGET model's module path for type resolution, not the parent's.
    // This ensures enum types like `AuthProvider` are resolved to `crate::models::user::AuthProvider`
    // instead of incorrectly using the parent module path.
    let target_module_path = extract_module_path_from_schema_path(&rel_info.schema_path);
    let effective_module_path = if target_module_path.is_empty() {
        source_module_path
    } else {
        &target_module_path
    };

    // Detect circular fields
    let circular_fields = detect_circular_fields("", source_module_path, model_def);

    // If no circular fields, no need for inline type
    if circular_fields.is_empty() {
        return None;
    }

    // Get rename_all from model (or default to camelCase)
    let rename_all =
        extract_rename_all(&parsed_model.attrs).unwrap_or_else(|| "camelCase".to_string());

    // Generate inline type name: {SchemaName}_{Field}
    // Use custom schema name if provided, otherwise use the Rust struct name
    let parent_name = match schema_name_override {
        Some(name) => name.to_string(),
        None => parent_type_name.to_string(),
    };
    let field_name_pascal = snake_to_pascal_case(&rel_info.field_name.to_string());
    let inline_type_name = syn::Ident::new(
        &format!("{}_{}", parent_name, field_name_pascal),
        proc_macro2::Span::call_site(),
    );

    // Collect fields, excluding circular ones and relation types
    let mut fields = Vec::new();
    if let syn::Fields::Named(fields_named) = &parsed_model.fields {
        for field in &fields_named.named {
            let field_ident = field.ident.as_ref()?;
            let field_name_str = field_ident.to_string();

            // Skip circular fields
            if circular_fields.contains(&field_name_str) {
                continue;
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
            // This prevents users from needing to import sea_orm::prelude::DateTimeWithTimeZone
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

/// Generate inline relation type for HasMany with ALL relations stripped.
///
/// When a HasMany relation is explicitly picked, the nested items should have
/// NO relation fields at all (not even FK relations). This prevents infinite
/// nesting and keeps the schema simple.
///
/// Example: If UserSchema picks "memos", each memo in the list will have
/// id, user_id, title, content, etc. but NO user or comments relations.
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
    // Parse the model struct
    let parsed_model: syn::ItemStruct = syn::parse_str(model_def).ok()?;

    // IMPORTANT: Use the TARGET model's module path for type resolution, not the parent's.
    // This ensures enum types like `StoryStatus` are resolved to `crate::models::story::StoryStatus`
    // instead of incorrectly using the parent module path.
    let target_module_path = extract_module_path_from_schema_path(&rel_info.schema_path);
    let effective_module_path = if target_module_path.is_empty() {
        source_module_path
    } else {
        &target_module_path
    };

    // Get rename_all from model (or default to camelCase)
    let rename_all =
        extract_rename_all(&parsed_model.attrs).unwrap_or_else(|| "camelCase".to_string());

    // Generate inline type name: {SchemaName}_{Field}
    let parent_name = match schema_name_override {
        Some(name) => name.to_string(),
        None => parent_type_name.to_string(),
    };
    let field_name_pascal = snake_to_pascal_case(&rel_info.field_name.to_string());
    let inline_type_name = syn::Ident::new(
        &format!("{}_{}", parent_name, field_name_pascal),
        proc_macro2::Span::call_site(),
    );

    // Collect fields, excluding ALL relation types
    let mut fields = Vec::new();
    if let syn::Fields::Named(fields_named) = &parsed_model.fields {
        for field in &fields_named.named {
            let field_ident = field.ident.as_ref()?;

            // Skip ALL relation types (HasOne, HasMany, BelongsTo)
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
            // This prevents users from needing to import sea_orm::prelude::DateTimeWithTimeZone
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

/// Generate the struct definition TokenStream for an inline relation type
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
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    fn test_generate_inline_type_definition() {
        let inline_type = InlineRelationType {
            type_name: syn::Ident::new("UserInline", proc_macro2::Span::call_site()),
            fields: vec![
                InlineField {
                    name: syn::Ident::new("id", proc_macro2::Span::call_site()),
                    ty: quote!(i32),
                    attrs: vec![],
                },
                InlineField {
                    name: syn::Ident::new("name", proc_macro2::Span::call_site()),
                    ty: quote!(String),
                    attrs: vec![],
                },
            ],
            rename_all: "camelCase".to_string(),
        };

        let tokens = generate_inline_type_definition(&inline_type);
        let output = tokens.to_string();

        assert!(output.contains("pub struct UserInline"));
        assert!(output.contains("pub id : i32"));
        assert!(output.contains("pub name : String"));
        assert!(output.contains("serde :: Serialize"));
        assert!(output.contains("serde :: Deserialize"));
        assert!(output.contains("vespera :: Schema"));
        assert!(output.contains("camelCase"));
    }

    #[test]
    fn test_generate_inline_type_definition_with_attrs() {
        let inline_type = InlineRelationType {
            type_name: syn::Ident::new("TestType", proc_macro2::Span::call_site()),
            fields: vec![InlineField {
                name: syn::Ident::new("field", proc_macro2::Span::call_site()),
                ty: quote!(String),
                attrs: vec![syn::parse_quote!(#[serde(rename = "renamed")])],
            }],
            rename_all: "snake_case".to_string(),
        };

        let tokens = generate_inline_type_definition(&inline_type);
        let output = tokens.to_string();

        assert!(output.contains("TestType"));
        assert!(output.contains("snake_case"));
    }

    #[test]
    fn test_generate_inline_type_definition_empty_fields() {
        let inline_type = InlineRelationType {
            type_name: syn::Ident::new("EmptyType", proc_macro2::Span::call_site()),
            fields: vec![],
            rename_all: "camelCase".to_string(),
        };

        let tokens = generate_inline_type_definition(&inline_type);
        let output = tokens.to_string();

        assert!(output.contains("pub struct EmptyType"));
        assert!(output.contains("Clone"));
        assert!(output.contains("vespera :: Schema"));
    }

    #[test]
    fn test_generate_inline_type_definition_multiple_attrs() {
        let inline_type = InlineRelationType {
            type_name: syn::Ident::new("MultiAttrType", proc_macro2::Span::call_site()),
            fields: vec![InlineField {
                name: syn::Ident::new("field", proc_macro2::Span::call_site()),
                ty: quote!(String),
                attrs: vec![
                    syn::parse_quote!(#[serde(default)]),
                    syn::parse_quote!(#[serde(skip_serializing_if = "Option::is_none")]),
                ],
            }],
            rename_all: "PascalCase".to_string(),
        };

        let tokens = generate_inline_type_definition(&inline_type);
        let output = tokens.to_string();

        assert!(output.contains("MultiAttrType"));
        assert!(output.contains("PascalCase"));
        assert!(output.contains("default"));
    }

    #[test]
    fn test_generate_inline_type_definition_complex_type() {
        let inline_type = InlineRelationType {
            type_name: syn::Ident::new("ComplexType", proc_macro2::Span::call_site()),
            fields: vec![
                InlineField {
                    name: syn::Ident::new("id", proc_macro2::Span::call_site()),
                    ty: quote!(i32),
                    attrs: vec![],
                },
                InlineField {
                    name: syn::Ident::new("tags", proc_macro2::Span::call_site()),
                    ty: quote!(Vec<String>),
                    attrs: vec![],
                },
                InlineField {
                    name: syn::Ident::new("metadata", proc_macro2::Span::call_site()),
                    ty: quote!(Option<std::collections::HashMap<String, serde_json::Value>>),
                    attrs: vec![],
                },
            ],
            rename_all: "camelCase".to_string(),
        };

        let tokens = generate_inline_type_definition(&inline_type);
        let output = tokens.to_string();

        assert!(output.contains("pub struct ComplexType"));
        assert!(output.contains("pub id : i32"));
        assert!(output.contains("Vec < String >"));
        assert!(output.contains("Option <"));
    }

    #[test]
    fn test_inline_field_struct() {
        // Test InlineField struct construction
        let field = InlineField {
            name: syn::Ident::new("test_field", proc_macro2::Span::call_site()),
            ty: quote!(Option<i32>),
            attrs: vec![syn::parse_quote!(#[doc = "Test doc"])],
        };

        assert_eq!(field.name.to_string(), "test_field");
        assert!(!field.attrs.is_empty());
    }

    #[test]
    fn test_inline_relation_type_struct() {
        // Test InlineRelationType struct construction
        let inline_type = InlineRelationType {
            type_name: syn::Ident::new("TestRelation", proc_macro2::Span::call_site()),
            fields: vec![],
            rename_all: "SCREAMING_SNAKE_CASE".to_string(),
        };

        assert_eq!(inline_type.type_name.to_string(), "TestRelation");
        assert_eq!(inline_type.rename_all, "SCREAMING_SNAKE_CASE");
        assert!(inline_type.fields.is_empty());
    }

    #[test]
    fn test_generate_inline_type_definition_doc_attr() {
        let inline_type = InlineRelationType {
            type_name: syn::Ident::new("DocType", proc_macro2::Span::call_site()),
            fields: vec![InlineField {
                name: syn::Ident::new("documented_field", proc_macro2::Span::call_site()),
                ty: quote!(String),
                attrs: vec![syn::parse_quote!(#[doc = "This is a documented field"])],
            }],
            rename_all: "camelCase".to_string(),
        };

        let tokens = generate_inline_type_definition(&inline_type);
        let output = tokens.to_string();

        assert!(output.contains("DocType"));
        assert!(output.contains("documented_field"));
        assert!(output.contains("doc"));
    }

    #[test]
    fn test_generate_inline_relation_type_from_def_with_circular() {
        // Test inline type generation when circular reference exists
        let parent_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(super::user::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        // UserSchema has a circular reference back to memo via HasMany
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub name: String,
            pub memos: HasMany<memo::Entity>
        }"#;

        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
            model_def,
        );
        // HasMany is not considered circular, so should return None
        assert!(result.is_none());

        // Test with BelongsTo instead (which IS considered circular)
        let model_def_with_belongs_to = r#"pub struct Model {
            pub id: i32,
            pub name: String,
            pub memo: BelongsTo<memo::Entity>
        }"#;

        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
            model_def_with_belongs_to,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();
        assert_eq!(inline_type.type_name.to_string(), "MemoSchema_User");
        // Should have id and name fields, but NOT memo (circular)
        let field_names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"name".to_string()));
        assert!(!field_names.contains(&"memo".to_string()));
    }

    #[test]
    fn test_generate_inline_relation_type_from_def_no_circular() {
        // Test that None is returned when no circular reference exists
        let parent_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("other", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(super::other::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "test".to_string(),
        ];

        // No circular reference
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub name: String
        }"#;

        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
            model_def,
        );
        assert!(result.is_none()); // No circular fields means no inline type needed
    }

    #[test]
    fn test_generate_inline_relation_type_from_def_with_schema_name_override() {
        let parent_type_name = syn::Ident::new("Schema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(super::user::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        let model_def = r#"pub struct Model {
            pub id: i32,
            pub memo: BelongsTo<memo::Entity>
        }"#;

        // With schema_name_override
        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            Some("MemoSchema"),
            model_def,
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().type_name.to_string(), "MemoSchema_User");
    }

    #[test]
    fn test_generate_inline_relation_type_no_relations_from_def() {
        let parent_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("memos", proc_macro2::Span::call_site()),
            relation_type: "HasMany".to_string(),
            schema_path: quote!(super::memo::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        // Model with relations that should be stripped
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub title: String,
            pub user: BelongsTo<user::Entity>,
            pub comments: HasMany<comment::Entity>
        }"#;

        let result = generate_inline_relation_type_no_relations_from_def(
            &parent_type_name,
            &rel_info,
            &[],
            None,
            model_def,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();
        assert_eq!(inline_type.type_name.to_string(), "UserSchema_Memos");

        // Should have id and title, but NOT user or comments (relations)
        let field_names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"title".to_string()));
        assert!(!field_names.contains(&"user".to_string()));
        assert!(!field_names.contains(&"comments".to_string()));
    }

    #[test]
    fn test_generate_inline_relation_type_no_relations_from_def_with_skip() {
        let parent_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("items", proc_macro2::Span::call_site()),
            relation_type: "HasMany".to_string(),
            schema_path: quote!(super::item::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        // Model with serde(skip) field
        let model_def = r#"pub struct Model {
            pub id: i32,
            #[serde(skip)]
            pub internal: String,
            pub name: String
        }"#;

        let result = generate_inline_relation_type_no_relations_from_def(
            &parent_type_name,
            &rel_info,
            &[],
            None,
            model_def,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();
        let field_names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"name".to_string()));
        assert!(!field_names.contains(&"internal".to_string())); // skipped
    }

    #[test]
    fn test_generate_inline_relation_type_from_def_invalid_model() {
        let parent_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(super::user::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec!["crate".to_string()];

        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
            "invalid rust code",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_generate_inline_relation_type_from_def_skips_relation_types() {
        // Test that relation types (HasOne, HasMany, BelongsTo) are skipped
        let parent_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(super::user::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        // Model with circular field AND other relation types that should be skipped
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub name: String,
            pub memo: BelongsTo<memo::Entity>,
            pub posts: HasMany<post::Entity>,
            pub profile: HasOne<profile::Entity>
        }"#;

        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
            model_def,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();
        let field_names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();

        // Should have id and name
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"name".to_string()));
        // Should NOT have any relation fields (circular or otherwise)
        assert!(!field_names.contains(&"memo".to_string())); // circular
        assert!(!field_names.contains(&"posts".to_string())); // HasMany - relation type
        assert!(!field_names.contains(&"profile".to_string())); // HasOne - relation type
    }

    #[test]
    fn test_generate_inline_relation_type_from_def_skips_serde_skip() {
        // Test that fields with serde(skip) are skipped
        let parent_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(super::user::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        // Model with circular field AND serde(skip) field
        let model_def = r#"pub struct Model {
            pub id: i32,
            #[serde(skip)]
            pub internal_cache: String,
            pub name: String,
            pub memo: BelongsTo<memo::Entity>
        }"#;

        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
            model_def,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();
        let field_names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();

        // Should have id and name
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"name".to_string()));
        // Should NOT have skipped or circular fields
        assert!(!field_names.contains(&"internal_cache".to_string())); // serde(skip)
        assert!(!field_names.contains(&"memo".to_string())); // circular
    }

    #[test]
    fn test_generate_inline_relation_type_no_relations_from_def_with_schema_name_override() {
        // Test schema_name_override Some branch
        let parent_type_name = syn::Ident::new("Schema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("memos", proc_macro2::Span::call_site()),
            relation_type: "HasMany".to_string(),
            schema_path: quote!(super::memo::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        let model_def = r#"pub struct Model {
            pub id: i32,
            pub title: String
        }"#;

        // With schema_name_override
        let result = generate_inline_relation_type_no_relations_from_def(
            &parent_type_name,
            &rel_info,
            &[],
            Some("UserSchema"),
            model_def,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();
        // Should use the override name, not the struct name
        assert_eq!(inline_type.type_name.to_string(), "UserSchema_Memos");
    }

    // Tests for public functions with file lookup
    // These require setting up a temp directory with model files

    #[test]
    #[serial]
    fn test_generate_inline_relation_type_with_file_lookup() {
        use tempfile::TempDir;

        // Create temp directory structure
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create a user.rs file with Model struct that has circular reference
        let user_model = r#"
pub struct Model {
    pub id: i32,
    pub name: String,
    pub memo: BelongsTo<memo::Entity>,
}
"#;
        std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

        // Save original CARGO_MANIFEST_DIR and set to temp dir
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Test generate_inline_relation_type
        let parent_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(crate::models::user::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        let result =
            generate_inline_relation_type(&parent_type_name, &rel_info, &source_module_path, None);

        // Restore original CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        // Verify result
        assert!(result.is_some());
        let inline_type = result.unwrap();
        assert_eq!(inline_type.type_name.to_string(), "MemoSchema_User");

        // Should have id and name, but not memo (circular)
        let field_names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"name".to_string()));
        assert!(!field_names.contains(&"memo".to_string()));
    }

    #[test]
    #[serial]
    fn test_generate_inline_relation_type_no_relations_with_file_lookup() {
        use tempfile::TempDir;

        // Create temp directory structure
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        let models_dir = src_dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();

        // Create a memo.rs file with Model struct that has relations
        let memo_model = r#"
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user: BelongsTo<user::Entity>,
    pub comments: HasMany<comment::Entity>,
}
"#;
        std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

        // Save original CARGO_MANIFEST_DIR and set to temp dir
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        // Test generate_inline_relation_type_no_relations
        let parent_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("memos", proc_macro2::Span::call_site()),
            relation_type: "HasMany".to_string(),
            schema_path: quote!(crate::models::memo::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let result = generate_inline_relation_type_no_relations(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
        );

        // Restore original CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        // Verify result
        assert!(result.is_some());
        let inline_type = result.unwrap();
        assert_eq!(inline_type.type_name.to_string(), "UserSchema_Memos");

        // Should have id and title, but not user or comments (relations)
        let field_names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"title".to_string()));
        assert!(!field_names.contains(&"user".to_string()));
        assert!(!field_names.contains(&"comments".to_string()));
    }

    #[test]
    #[serial]
    fn test_generate_inline_relation_type_file_not_found() {
        use tempfile::TempDir;

        // Create temp directory structure without the model file
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        // Save original CARGO_MANIFEST_DIR and set to temp dir
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let parent_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(crate::models::nonexistent::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec!["crate".to_string()];

        let result =
            generate_inline_relation_type(&parent_type_name, &rel_info, &source_module_path, None);

        // Restore original CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        // Should return None when file not found
        assert!(result.is_none());
    }

    #[test]
    #[serial]
    fn test_generate_inline_relation_type_no_relations_file_not_found() {
        use tempfile::TempDir;

        // Create temp directory structure without the model file
        let temp_dir = TempDir::new().unwrap();
        let src_dir = temp_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        // Save original CARGO_MANIFEST_DIR and set to temp dir
        let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: This is a test that runs single-threaded
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

        let parent_type_name = syn::Ident::new("TestSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("items", proc_macro2::Span::call_site()),
            relation_type: "HasMany".to_string(),
            schema_path: quote!(crate::models::nonexistent::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        let result =
            generate_inline_relation_type_no_relations(&parent_type_name, &rel_info, &[], None);

        // Restore original CARGO_MANIFEST_DIR
        // SAFETY: This is a test that runs single-threaded
        unsafe {
            if let Some(dir) = original_manifest_dir {
                std::env::set_var("CARGO_MANIFEST_DIR", dir);
            } else {
                std::env::remove_var("CARGO_MANIFEST_DIR");
            }
        }

        // Should return None when file not found
        assert!(result.is_none());
    }

    #[test]
    fn test_generate_inline_relation_type_converts_datetime_types() {
        // Test that DateTimeWithTimeZone is converted to vespera::chrono::DateTime<FixedOffset>
        let parent_type_name = syn::Ident::new("MemoSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "BelongsTo".to_string(),
            schema_path: quote!(super::user::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        let source_module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];

        // Model with DateTimeWithTimeZone field AND circular reference
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub name: String,
            pub created_at: DateTimeWithTimeZone,
            pub memo: BelongsTo<memo::Entity>
        }"#;

        let result = generate_inline_relation_type_from_def(
            &parent_type_name,
            &rel_info,
            &source_module_path,
            None,
            model_def,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();
        assert_eq!(inline_type.type_name.to_string(), "MemoSchema_User");

        // Find created_at field and check its type was converted
        let created_at_field = inline_type
            .fields
            .iter()
            .find(|f| f.name == "created_at")
            .expect("created_at field should exist");

        let ty_str = created_at_field.ty.to_string();
        // Should be converted to vespera::chrono::DateTime<FixedOffset>
        assert!(
            ty_str.contains("vespera :: chrono :: DateTime"),
            "DateTimeWithTimeZone should be converted to vespera::chrono::DateTime, got: {}",
            ty_str
        );
        assert!(
            ty_str.contains("FixedOffset"),
            "Should contain FixedOffset, got: {}",
            ty_str
        );
    }

    #[test]
    fn test_generate_inline_relation_type_no_relations_converts_datetime_types() {
        // Test that DateTimeWithTimeZone is converted in no_relations variant too
        let parent_type_name = syn::Ident::new("UserSchema", proc_macro2::Span::call_site());
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("memos", proc_macro2::Span::call_site()),
            relation_type: "HasMany".to_string(),
            schema_path: quote!(super::memo::Schema),
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };

        // Model with DateTimeWithTimeZone field
        let model_def = r#"pub struct Model {
            pub id: i32,
            pub title: String,
            pub created_at: DateTimeWithTimeZone,
            pub updated_at: Option<DateTimeWithTimeZone>,
            pub user: BelongsTo<user::Entity>
        }"#;

        let result = generate_inline_relation_type_no_relations_from_def(
            &parent_type_name,
            &rel_info,
            &[],
            None,
            model_def,
        );
        assert!(result.is_some());

        let inline_type = result.unwrap();

        // Find created_at field and check its type was converted
        let created_at_field = inline_type
            .fields
            .iter()
            .find(|f| f.name == "created_at")
            .expect("created_at field should exist");

        let ty_str = created_at_field.ty.to_string();
        assert!(
            ty_str.contains("vespera :: chrono :: DateTime"),
            "DateTimeWithTimeZone should be converted, got: {}",
            ty_str
        );

        // Also check Option<DateTimeWithTimeZone>
        let updated_at_field = inline_type
            .fields
            .iter()
            .find(|f| f.name == "updated_at")
            .expect("updated_at field should exist");

        let updated_ty_str = updated_at_field.ty.to_string();
        assert!(
            updated_ty_str.contains("Option"),
            "Should be Option type, got: {}",
            updated_ty_str
        );
        assert!(
            updated_ty_str.contains("vespera :: chrono :: DateTime"),
            "Option<DateTimeWithTimeZone> should be converted, got: {}",
            updated_ty_str
        );
    }
}
