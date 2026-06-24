//! Schema macro implementation
//!
//! Provides macros for generating `OpenAPI` schemas from struct types:
//! - `schema!` - Generate Schema value with optional field filtering
//! - `schema_type!` - Generate new struct type derived from existing type

mod circular;
mod codegen;
mod defaults;
pub mod file_cache;
mod file_lookup;
mod from_model;
mod generate_type;
mod inline_types;
mod input;
mod same_file_override;
mod seaorm;
mod transformation;
pub mod type_utils;
mod validation;

pub use file_cache::print_profile_summary;
pub use generate_type::generate_schema_type_code;
pub use input::{SchemaInput, SchemaTypeInput};

use std::collections::{HashMap, HashSet};

use codegen::generate_filtered_schema;
use proc_macro2::TokenStream;
use type_utils::extract_type_name;

use crate::metadata::StructMetadata;

/// Generate schema code from a struct with optional field filtering
pub fn generate_schema_code(
    input: &SchemaInput,
    schema_storage: &HashMap<String, StructMetadata>,
) -> Result<TokenStream, syn::Error> {
    // Extract type name from the Type
    let type_name = extract_type_name(&input.ty)?;

    // Find struct definition in storage (O(1) HashMap lookup)
    let struct_def = schema_storage.get(&type_name).ok_or_else(|| syn::Error::new_spanned(&input.ty, format!("type `{type_name}` not found. Make sure it has #[derive(Schema)] before this macro invocation")))?;

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = file_cache::parse_struct_cached(&struct_def.definition)
        .map_err(|e| {
            syn::Error::new_spanned(
                &input.ty,
                format!("failed to parse struct definition for `{type_name}`: {e}"),
            )
        })?;

    // Build omit set
    let omit_set: HashSet<String> = input.omit.iter().flatten().cloned().collect();

    // Build pick set
    let pick_set: HashSet<String> = input.pick.iter().flatten().cloned().collect();

    // Generate schema with filtering
    let schema_tokens =
        generate_filtered_schema(&parsed_struct, &omit_set, &pick_set, schema_storage);

    Ok(schema_tokens)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use quote::quote;

    use super::defaults::is_parseable_type;
    use super::same_file_override::maybe_generate_same_file_relation_override;
    use super::seaorm::RelationFieldInfo;
    use super::*;

    fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
        StructMetadata::new(name.to_string(), definition.to_string())
    }

    fn to_storage(items: Vec<StructMetadata>) -> HashMap<String, StructMetadata> {
        items.into_iter().map(|s| (s.name.clone(), s)).collect()
    }

    #[test]
    fn test_generate_schema_code_simple_struct() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

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
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )]);

        let tokens = quote!(User, omit = ["password"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
    }

    #[test]
    fn test_generate_schema_code_with_pick() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub email: String }",
        )]);

        let tokens = quote!(User, pick = ["id", "name"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
    }

    #[test]
    fn test_generate_schema_code_type_not_found() {
        let storage: HashMap<String, StructMetadata> = HashMap::new();

        let tokens = quote!(NonExistent);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_code_malformed_definition() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "BadStruct",
            "this is not valid rust code {{{",
        )]);

        let tokens = quote!(BadStruct);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to parse"));
    }

    #[test]
    fn test_generate_schema_type_code_pick_nonexistent_field() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

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
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

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
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

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
        let storage: HashMap<String, StructMetadata> = HashMap::new();

        let tokens = quote!(NewUser from NonExistent);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_type_code_success() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

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
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )]);

        let tokens = quote!(SafeUser from User, omit = ["password"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("SafeUser"));
        assert!(!output.contains("password"));
    }

    #[test]
    fn test_generate_schema_type_code_with_add() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

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
    fn test_generate_schema_type_code_relation_fields_can_be_omitted_and_readded_with_custom_types()
    {
        let storage = to_storage(vec![create_test_struct_metadata(
            "Model",
            r#"#[sea_orm(table_name = "article")]
            pub struct Model {
                pub id: i64,
                pub title: String,
                pub user: HasOne<super::user::Entity>,
                pub category: HasOne<super::category::Entity>,
                pub article_review_users: HasMany<super::article_review_user::Entity>
            }"#,
        )]);

        let tokens = quote!(
            ArticleResponse from Model,
            omit = ["user", "category", "article_review_users"],
            add = [
                ("user": Option<UserInArticle>),
                ("category": Option<CategoryInArticle>),
                ("article_review_users": Vec<ArticleReviewUserInArticle>)
            ]
        );
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("pub user : Option < UserInArticle >"));
        assert!(output.contains("pub category : Option < CategoryInArticle >"));
        assert!(output.contains("pub article_review_users : Vec < ArticleReviewUserInArticle >"));
        assert!(!output.contains("Box < Schema >"));
        assert!(!output.contains("impl From"));
    }

    #[test]
    fn test_generate_schema_type_code_same_file_relation_adapters_when_explicit() {
        let storage = to_storage(vec![
            create_test_struct_metadata(
                "Model",
                r#"#[sea_orm(table_name = "article")]
                pub struct Model {
                    pub id: i64,
                    pub title: String,
                    pub user: HasOne<super::user::Entity>,
                    pub category: HasOne<super::category::Entity>,
                    pub article_review_users: HasMany<super::article_review_user::Entity>
                }"#,
            ),
            create_test_struct_metadata(
                "UserInArticle",
                "struct UserInArticle { id: i32, name: String }",
            ),
            create_test_struct_metadata(
                "CategoryInArticle",
                "struct CategoryInArticle { id: i64, name: String }",
            ),
        ]);

        let tokens = quote!(
            ArticleResponse from Model,
            relation_adapters = [("user", UserInArticle), ("category", CategoryInArticle)],
            add = [("article_review_users": Vec<ArticleReviewUserInArticle>)]
        );
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(output.contains("pub user : __VesperaArticleResponseUserRelation"));
        assert!(output.contains("pub category : __VesperaArticleResponseCategoryRelation"));
        assert!(output.contains("impl From < Option <"));
        assert!(output.contains("for __VesperaArticleResponseUserRelation"));
        assert!(output.contains("for __VesperaArticleResponseCategoryRelation"));
        assert!(output.contains("impl Clone for UserInArticle"));
        assert!(output.contains("impl Clone for CategoryInArticle"));
    }

    #[test]
    fn test_maybe_generate_same_file_relation_override_skips_redundant_clone_and_deserialize_impls()
    {
        // Same-file relation override DTOs that ALREADY carry `Clone` and
        // `Deserialize` derives must NOT have the macro re-emit those
        // impls — otherwise the generated code would conflict with the
        // user-provided derive.  Hits the "DTO already has derive" empty-
        // quote branches inside `maybe_generate_same_file_relation_override`.
        let rel_info = RelationFieldInfo {
            field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
            relation_type: "HasOne".to_string(),
            schema_path: quote!(crate::models::user::Schema),
            is_optional: true,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        };
        // Bare `Clone` and `Deserialize` idents — has_derive matches the
        // single-segment path, hitting the empty-quote branches at lines
        // 208 (clone_impl) and 222 (deserialize_impl).
        let storage = to_storage(vec![create_test_struct_metadata(
            "UserInArticle",
            r"#[derive(Clone, Deserialize)]
            struct UserInArticle { id: i32, name: String }",
        )]);
        let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
        let adapter_name = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());

        let (override_field_ty, helper_tokens) = maybe_generate_same_file_relation_override(
            &new_type_name,
            "user",
            &adapter_name,
            &rel_info,
            &storage,
        )
        .expect("override generation should succeed");

        let output = helper_tokens.to_string();
        let field_ty = override_field_ty.to_string();
        assert!(
            field_ty.contains("__VesperaArticleResponseUserRelation"),
            "expected override field type to reference relation adapter, got: {field_ty}"
        );
        // No `impl Clone for UserInArticle` — DTO already derives Clone.
        assert!(
            !output.contains("impl Clone for UserInArticle"),
            "macro should skip Clone impl when DTO already derives Clone, got: {output}"
        );
        // No proxy `Deserialize` derive struct — DTO already derives Deserialize.
        assert!(
            !output.contains("__VesperaArticleResponseUserProxy"),
            "macro should skip Deserialize proxy when DTO already derives Deserialize, got: {output}"
        );
        // Relation wrapper struct still emitted regardless of derives.
        assert!(
            output.contains("__VesperaArticleResponseUserRelation"),
            "relation wrapper missing: {output}"
        );
    }

    #[test]
    fn test_generate_schema_type_code_generates_from_impl() {
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

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
        let storage = to_storage(vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )]);

        let tokens = quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let (tokens, _metadata) = result.unwrap();
        let output = tokens.to_string();
        assert!(
            output.contains("UserWithExtra"),
            "expected struct UserWithExtra in output: {output}"
        );
        assert!(
            !output.contains("impl From"),
            "expected no From impl when `add` is used: {output}"
        );
    }

    // ========================
    // is_parseable_type tests
    // ========================

    #[test]
    fn test_is_parseable_type_primitives() {
        for ty_str in &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
            "f32", "f64", "bool", "String", "Decimal",
        ] {
            let ty: syn::Type = syn::parse_str(ty_str).unwrap();
            assert!(is_parseable_type(&ty), "{ty_str} should be parseable");
        }
    }

    #[test]
    fn test_is_parseable_type_non_parseable() {
        let ty: syn::Type = syn::parse_str("MyEnum").unwrap();
        assert!(!is_parseable_type(&ty));
    }

    #[test]
    fn test_is_parseable_type_non_path() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        assert!(!is_parseable_type(&ty));
    }
}
