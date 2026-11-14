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

#[test]
fn malformed_stored_definition_returns_spanned_parse_error() {
    let storage = to_storage(vec![create_test_struct_metadata("Broken", "not a struct")]);
    let input: SchemaTypeInput = syn::parse2(quote!(Output from Broken)).unwrap();

    let error = generate_schema_type_code(&input, &storage)
        .expect_err("malformed stored definitions must be diagnosed");
    assert!(
        error
            .to_string()
            .starts_with("failed to parse struct definition for `Broken`:")
    );
}

#[test]
fn explicit_relation_adapter_generates_override_field_and_helper() {
    let storage = to_storage(vec![
        create_test_struct_metadata(
            "Model",
            r#"#[sea_orm(table_name = "article")]
               pub struct Model { pub user: HasOne<super::user::Entity> }"#,
        ),
        create_test_struct_metadata("UserAdapter", "pub struct UserAdapter { pub id: i64 }"),
    ]);
    let input: SchemaTypeInput = syn::parse2(quote!(
        Article from Model, relation_adapters = [("user", UserAdapter)]
    ))
    .unwrap();

    let (tokens, _) = generate_schema_type_code(&input, &storage).unwrap();
    let output = tokens.to_string();
    assert!(output.contains("pub user : __VesperaArticleUserRelation"));
    assert!(output.contains("impl From < Option <"));
}

#[test]
fn explicitly_picked_unconvertible_relation_returns_error() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "article")]
           pub struct Model { pub user: HasOne }"#,
    )]);
    let input: SchemaTypeInput = syn::parse2(quote!(Article from Model, pick = ["user"])).unwrap();

    let error = generate_schema_type_code(&input, &storage)
        .expect_err("a picked relation without a target type cannot be converted");
    assert_eq!(
        error.to_string(),
        "schema_type!: relation field `user` was explicitly picked but its SeaORM relation type could not be converted"
    );
}

#[test]
fn normal_rename_maps_generated_field_back_to_source_field() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub original: String }",
    )]);
    let input: SchemaTypeInput =
        syn::parse2(quote!(Renamed from User, rename = [("original", "renamed")])).unwrap();

    let (tokens, _) = generate_schema_type_code(&input, &storage).unwrap();
    let output = tokens.to_string();
    assert!(output.contains("pub renamed : String"));
    assert!(output.contains("renamed : source . original"));
}

#[test]
fn malformed_sea_orm_from_is_a_compile_error_not_a_panic() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "posts")]
        pub struct Model {
            pub id: i32,
            pub author_id: i32,
            #[sea_orm(belongs_to = "super::user::Entity", from = "author id", to = "id", relation_enum = "Author")]
            pub author: BelongsTo<super::user::Entity>,
        }"#,
    )]);
    let input: SchemaTypeInput = syn::parse2(quote!(PostSchema from Model)).unwrap();

    let error = generate_schema_type_code(&input, &storage).unwrap_err();
    assert_eq!(
        error.to_string(),
        "schema_type!: relation field `author` has `#[sea_orm(from = \"author id\")]`, but `author id` is not a valid Rust identifier"
    );
}
