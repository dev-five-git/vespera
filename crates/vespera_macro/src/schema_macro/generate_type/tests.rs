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
    // The Rust field is renamed (no `file_path` field declaration), while its
    // form field keeps the source's raw name, which the source's Multipart
    // derive reads (R23-CJ1-M1; it used to read the camelCase `filePath`).
    assert!(!output.contains("file_path :"), "{output}");
    assert!(output.contains("field_name = \"file_path\""), "{output}");
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

/// L14: a raw new type name (`r#Foo`) went verbatim into generated fn
/// names (`default_r#Foo_count`): `Ident::new` panicked.
#[test]
fn raw_type_name_in_generated_fn_names() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i32,
            #[sea_orm(default_value = "5")]
            pub count: i32,
            #[serde(with = "codec")]
            pub at: String,
        }"#,
    )]);
    let input: SchemaTypeInput = syn::parse_str("r#Foo from Model").unwrap();
    let output = generate_schema_type_code(&input, &storage)
        .unwrap()
        .0
        .to_string();
    assert!(output.contains("fn default_Foo_count"), "{output}");
    let input: SchemaTypeInput = syn::parse_str("r#Foo from Model, partial").unwrap();
    let output = generate_schema_type_code(&input, &storage)
        .unwrap()
        .0
        .to_string();
    assert!(output.contains("fn __vespera_Foo_at_ser"), "{output}");
}

/// M22: `rename` / `partial` / `pick` naming a field `omit` / `pick` keep
/// but the type still drops used to be silently ignored.
#[rstest::rstest]
#[case::rename_omit_default(
    "Dto from Model, omit_default, rename = [(\"id\", \"key\")]",
    "`rename` names field `id`, which this type does not include: `omit_default` drops it (a SeaORM default / primary key)"
)]
#[case::partial_omit_default(
    "Dto from Model, omit_default, partial = [\"count\"]",
    "`partial` names field `count`, which this type does not include: `omit_default` drops it (a SeaORM default / primary key)"
)]
#[case::pick_omit_default(
    "Dto from Model, omit_default, pick = [\"id\", \"name\"]",
    "`pick` names field `id`, which this type does not include: `omit_default` drops it (a SeaORM default / primary key)"
)]
#[case::rename_multipart_relation(
    "Dto from Model, multipart, rename = [(\"owner\", \"o\")]",
    "`rename` names field `owner`, which this type does not include: `multipart` types carry no relations"
)]
#[case::rename_unpicked_has_many(
    "Dto from Model, rename = [(\"items\", \"all\")]",
    "`rename` names field `items`, which this type does not include: a HasMany relation is only included when `pick`ed"
)]
#[case::partial_unconvertible_relation(
    "Dto from Model, partial = [\"broken\"]",
    "`partial` names field `broken`, which this type does not include: its SeaORM relation type could not be converted"
)]
// An included relation is never wrapped: naming it is an error, not a no-op.
#[case::partial_included_relation(
    "Dto from Model, partial = [\"owner\"]",
    "`partial` names the SeaORM relation `owner`, which `partial` never makes optional: omit it (`omit = [\"owner\"]`) or declare the patch field with `add`"
)]
fn naming_a_dropped_field_is_an_error(#[case] input: &str, #[case] expected: &str) {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "things")]
        pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i32,
            pub name: String,
            #[sea_orm(default_value = "0")]
            pub count: i32,
            pub owner: HasOne<super::user::Entity>,
            pub items: HasMany<super::item::Entity>,
            pub broken: HasOne,
        }"#,
    )]);
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    let error = generate_schema_type_code(&input, &storage).unwrap_err();
    assert_eq!(error.to_string(), expected);
}

/// R28-SM2-L1: `omit_default` drops the primary-key twin of a `#[cfg]` pair,
/// but the other twin is emitted in every build, so the name stays valid for
/// `rename` / `pick` / `partial`.
#[rstest::rstest]
#[case::rename(
    "Dto from Model, omit_default, rename = [(\"id\", \"key\")]",
    "pub key"
)]
#[case::pick("Dto from Model, omit_default, pick = [\"id\"]", "pub id")]
#[case::partial("Dto from Model, omit_default, partial = [\"id\"]", "Option")]
fn cfg_twin_dropped_by_omit_default_keeps_the_name(#[case] input: &str, #[case] expected: &str) {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"pub struct Model {
            #[cfg(feature = "pg")]
            #[sea_orm(primary_key)]
            pub id: i64,
            #[cfg(not(feature = "pg"))]
            pub id: i64,
            pub name: String,
        }"#,
    )]);
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    let output = generate_schema_type_code(&input, &storage)
        .unwrap_or_else(|error| panic!("{error}"))
        .0
        .to_string();
    assert!(output.contains(expected), "{output}");
}

/// R28-SM4-M1 (the `schema_type!` twin of `schema!`'s
/// `filter_name_ignores_skipped_fields`): the serde-skipped `id` does not take
/// the name `id` from `public_id`, whose JSON name is `id`.
#[rstest::rstest]
#[case::omit_drops_the_renamed_field("Dto from Model, omit = [\"id\"]", "label", "public_id")]
#[case::pick_selects_the_renamed_field("Dto from Model, pick = [\"id\"]", "public_id", "label")]
fn skipped_identifier_does_not_shadow_a_json_name(
    #[case] input: &str,
    #[case] kept: &str,
    #[case] dropped: &str,
) {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[derive(serde::Serialize)]
        pub struct Model {
            #[serde(skip)]
            pub id: i64,
            #[serde(rename = "id")]
            pub public_id: i64,
            pub label: String,
        }"#,
    )]);
    let input: SchemaTypeInput = syn::parse_str(input).unwrap();
    let output = generate_schema_type_code(&input, &storage)
        .unwrap_or_else(|error| panic!("{error}"))
        .0
        .to_string();
    assert!(output.contains(kept), "{output}");
    assert!(!output.contains(dropped), "{output}");
}
