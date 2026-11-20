//! Unit tests of the same-file relation override (moved out of the parent
//! to keep it under the 1000-line cap, SM35-3-L1).

use std::collections::HashMap;

use quote::quote;

use super::*;
use crate::metadata::StructMetadata;
use crate::schema_macro::seaorm::RelationFieldInfo;
use crate::schema_macro::{SchemaTypeInput, generate_schema_type_code};

fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
    StructMetadata::new(name.to_string(), definition.to_string())
}

fn to_storage(items: Vec<StructMetadata>) -> HashMap<String, StructMetadata> {
    items.into_iter().map(|s| (s.name.clone(), s)).collect()
}

#[test]
fn test_find_same_file_struct_metadata_reads_test_fixture_from_current_module() {
    let storage: HashMap<String, StructMetadata> = HashMap::new();
    let metadata = find_same_file_struct_metadata("__VesperaSameFileLookupFixture", &storage)
        .expect("fixture should be found in schema_macro/same_file_override.rs");

    assert_eq!(metadata.name, "__VesperaSameFileLookupFixture");
    assert!(
        metadata
            .definition
            .contains("__VesperaSameFileLookupFixture")
    );
    assert!(metadata.definition.contains("value"));
}

/// R20-SM3-01 (round 20) kept the stored `Schema` over a plain struct of
/// the adapter's name in the call-site file. The wrapper names that file
/// struct, so the stored fields cannot build it (R21-SM3-02): the adapter
/// is rejected with a diagnostic asking for `#[derive(Schema)]` there.
#[test]
fn explicit_adapter_rejects_an_underived_file_struct_the_storage_would_replace() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "__VesperaAdapterFixture",
        "struct __VesperaAdapterFixture { value: i32 }",
    )]);
    let ident = syn::Ident::new("__VesperaAdapterFixture", proc_macro2::Span::call_site());
    let Err(error) = explicit_adapter_struct("author", &ident, &storage) else {
        panic!("a plain call-site struct must not take the stored fields")
    };
    assert!(
        error.to_string().contains("does not derive `Schema`"),
        "{error}"
    );
}

/// The file's `Schema`-derived DTO beats a same-named stored entry, and
/// a same-named file struct without `Schema` only fills an empty storage.
#[rstest::rstest]
#[case::derived_file_item_wins(
    Some("#[derive(Serialize, vespera::Schema)] struct Dto { id: i32 }"),
    "struct Dto { value: i32 }",
    "id"
)]
#[case::underived_file_item_yields(
    Some("#[derive(Serialize)] struct Dto { id: i32 }"),
    "struct Dto { value: i32 }",
    "value"
)]
#[case::unparseable_file_item_yields(Some("struct Dto {"), "struct Dto { value: i32 }", "value")]
fn file_definition_beats_storage_only_when_it_is_a_schema(
    #[case] file: Option<&str>,
    #[case] stored: &str,
    #[case] expected_field: &str,
) {
    let stored = create_test_struct_metadata("Dto", stored);
    let chosen = choose_same_file("Dto", file.map(str::to_string), Some(&stored)).unwrap();
    assert!(
        chosen.definition.contains(expected_field),
        "{}",
        chosen.definition
    );
}

#[test]
fn underived_file_item_fills_an_empty_storage() {
    let chosen = choose_same_file("Dto", Some("struct Dto { id: i32 }".to_string()), None).unwrap();
    assert!(chosen.definition.contains("id"));
}

/// A name the call-site file does not define falls back to the storage.
#[test]
fn explicit_adapter_falls_back_to_storage_when_the_file_lacks_it() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "__VesperaStorageOnly",
        "struct __VesperaStorageOnly { pub value: i32 }",
    )]);
    let ident = syn::Ident::new("__VesperaStorageOnly", proc_macro2::Span::call_site());
    let (name, _, dto) = explicit_adapter_struct("author", &ident, &storage).unwrap();
    assert_eq!(name, "__VesperaStorageOnly");
    assert_eq!(dto.ident.to_string(), "__VesperaStorageOnly");
}

/// R23-SM3-02: a storage-fallback DTO whose field type (`Kind`) only its
/// own module resolves is refused with a spanned error, not copied into
/// the call-site proxy where it would not compile. A primitive or prelude
/// field stays accepted (the test above).
#[test]
fn explicit_adapter_refuses_a_field_type_only_its_module_resolves() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "__VesperaKindOwner",
        "struct __VesperaKindOwner { kind: Kind }",
    )]);
    let ident = syn::Ident::new("__VesperaKindOwner", proc_macro2::Span::call_site());
    let Err(error) = explicit_adapter_struct("author", &ident, &storage) else {
        panic!("the adapter should be refused");
    };
    assert!(error.to_string().contains("names `Kind`"), "{error}");
}

/// R23-SM3-02: a `serde(default = "helper")` of a storage-fallback DTO is
/// the same kind of path (a bare name the call site may not see).
#[test]
fn explicit_adapter_refuses_a_serde_default_only_its_module_resolves() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "__VesperaDefaultOwner",
        "struct __VesperaDefaultOwner { #[serde(default = \"make_value\")] value: i32 }",
    )]);
    let ident = syn::Ident::new("__VesperaDefaultOwner", proc_macro2::Span::call_site());
    let Err(error) = explicit_adapter_struct("author", &ident, &storage) else {
        panic!("the adapter should be refused");
    };
    assert!(error.to_string().contains("names `make_value`"), "{error}");
}

/// R23-SM3-02: a `self::` / `super::` default function is relative to the
/// DTO's module too, and it is refused the same way.
#[test]
fn explicit_adapter_refuses_a_relative_serde_default_path() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "__VesperaRelativeOwner",
        "struct __VesperaRelativeOwner { #[serde(default = \"super::make_value\")] value: i32 }",
    )]);
    let ident = syn::Ident::new("__VesperaRelativeOwner", proc_macro2::Span::call_site());
    let Err(error) = explicit_adapter_struct("author", &ident, &storage) else {
        panic!("the adapter should be refused");
    };
    assert!(
        error.to_string().contains("names `super::make_value`"),
        "{error}"
    );
}

#[test]
fn named_fields_rejects_tuple_structs() {
    let struct_item: syn::ItemStruct = syn::parse_str("struct TupleDto(String);").unwrap();
    let error = named_fields(&struct_item).unwrap_err();
    assert!(error.to_string().contains("named-field struct"));
}

fn user_relation() -> RelationFieldInfo {
    RelationFieldInfo {
        field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
        relation_type: "HasOne".to_string(),
        schema_path: quote!(crate::models::user::Schema),
        is_optional: true,
        inline_type_info: None,
        relation_enum: None,
        fk_column: None,
        fk_optional: true,
        fk_to: None,
        via_rel: None,
        adapter: false,
    }
}

/// The helpers for adapter DTOs: every impl lives on the wrapper (no
/// `impl From<Model> for Dto`, so one DTO can adapt several fields),
/// `#[cfg]` fields keep their cfg on each assignment, and the container
/// conversion keys shape the `Deserialize`.
#[rstest::rstest]
#[case::plain(
    "adapter_plain",
    "struct UserInArticle { pub id: i32, pub name: String }"
)]
#[case::cfg_field(
    "adapter_cfg_field",
    "struct UserInArticle { pub id: i32, #[cfg(feature = \"x\")] #[serde(rename = \"n\")] pub name: String }"
)]
#[case::container_from(
    "adapter_container_from",
    "#[serde(from = \"String\", into = \"String\", rename_all = \"camelCase\")] struct UserInArticle { pub id: i32 }"
)]
#[case::container_try_from(
    "adapter_container_try_from",
    "#[serde(try_from = \"String\")] struct UserInArticle { pub id: i32 }"
)]
#[case::container_default(
    "adapter_container_default",
    "#[serde(default, bound = \"\", deny_unknown_fields)] struct UserInArticle { pub id: i32, #[cfg(test)] pub name: String }"
)]
#[case::container_default_path(
    "adapter_container_default_path",
    "#[serde(default = \"Self::fallback\")] struct UserInArticle { pub id: i32 }"
)]
// L13 (round 12): `cfg_attr` serde items reach the proxy (non-serde items
// and unparseable ones are dropped). A cfg-gated conversion key or schema
// name under an undecided predicate is refused instead (SM35-3-L2, see
// `adapter_ref::reject_undecided_cfg_attr`).
#[case::cfg_attr_serde(
    "adapter_cfg_attr_serde",
    "#[cfg_attr(feature = \"x\", serde(rename_all = \"camelCase\"))] #[cfg_attr(all(), serde(default))] #[cfg_attr(feature)] struct UserInArticle { #[cfg_attr(feature = \"x\", serde(rename = \"uid\"), garde(skip))] pub user_id: i32, #[cfg_attr(all(), serde, schema(minimum = 1))] pub other: i32 }"
)]
// L11 (round 16): a `cfg_attr(p, cfg(q))` field gates its assignments
// and its proxy field (both were unconditional: E0560).
#[case::cfg_attr_cfg(
    "adapter_cfg_attr_cfg",
    "struct UserInArticle { pub id: i32, #[cfg_attr(feature = \"a\", cfg(feature = \"b\"), serde(rename = \"n\"))] pub name: String }"
)]
// L3 (round 17): `Self` in the DTO's serde paths and field types names
// the DTO, not the proxy (`<Proxy>::default_name`: E0599).
#[case::self_paths(
    "adapter_self_paths",
    "#[serde(default = \"Self::make\")] struct UserInArticle { #[serde(default = \"Self::default_name\")] pub name: String, pub next: Option<Box<Self>> }"
)]
fn adapter_helpers_snapshot(#[case] name: &str, #[case] definition: &str) {
    let storage = to_storage(vec![create_test_struct_metadata(
        "UserInArticle",
        definition,
    )]);
    let (_, helpers) = maybe_generate_same_file_relation_override(
        &syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site()),
        "user",
        &syn::Ident::new("UserInArticle", proc_macro2::Span::call_site()),
        &user_relation(),
        &storage,
        true,
    )
    .unwrap();
    let file: syn::File = syn::parse2(helpers).unwrap();
    insta::assert_snapshot!(name, prettyplease::unparse(&file));
}

#[test]
fn adapter_requires_named_fields() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "UserInArticle",
        "struct UserInArticle(i32);",
    )]);
    let error = maybe_generate_same_file_relation_override(
        &syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site()),
        "user",
        &syn::Ident::new("UserInArticle", proc_macro2::Span::call_site()),
        &user_relation(),
        &storage,
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("named-field struct"));
}

/// `clone = false`: no wrapper `Clone` (a non-`Clone` adapter field would
/// fail it); an unparseable stored definition names the adapter.
#[test]
fn adapter_clone_follows_derive_clone_and_parse_errors_name_the_adapter() {
    let generate = |definition: &str, derive_clone: bool| {
        let storage = to_storage(vec![create_test_struct_metadata(
            "UserInArticle",
            definition,
        )]);
        maybe_generate_same_file_relation_override(
            &syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site()),
            "user",
            &syn::Ident::new("UserInArticle", proc_macro2::Span::call_site()),
            &user_relation(),
            &storage,
            derive_clone,
        )
    };
    let definition = "struct UserInArticle { pub id: i32 }";
    let (_, with) = generate(definition, true).unwrap();
    let (_, without) = generate(definition, false).unwrap();
    assert!(with.to_string().contains("impl Clone for"));
    assert!(!without.to_string().contains("impl Clone for"));
    let error = generate("struct UserInArticle {", true)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("`user`") && error.contains("`UserInArticle`"),
        "{error}"
    );
}

#[test]
fn test_maybe_generate_same_file_relation_override_errors_when_explicit_dto_is_missing() {
    let rel_info = RelationFieldInfo {
        field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
        relation_type: "HasOne".to_string(),
        schema_path: quote!(crate::models::user::Schema),
        is_optional: true,
        inline_type_info: None,
        relation_enum: None,
        fk_column: None,
        fk_optional: true,
        fk_to: None,
        via_rel: None,
        adapter: false,
    };

    let storage: HashMap<String, StructMetadata> = HashMap::new();
    let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
    let adapter_name = syn::Ident::new("MissingUserAdapter", proc_macro2::Span::call_site());

    let error = maybe_generate_same_file_relation_override(
        &new_type_name,
        "user",
        &adapter_name,
        &rel_info,
        &storage,
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("MissingUserAdapter"));
    assert!(error.to_string().contains("relation field `user`"));
}

#[test]
fn test_maybe_generate_same_file_relation_override_errors_for_invalid_model_type() {
    let rel_info = RelationFieldInfo {
        field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
        relation_type: "HasOne".to_string(),
        schema_path: quote!(?),
        is_optional: true,
        inline_type_info: None,
        relation_enum: None,
        fk_column: None,
        fk_optional: true,
        fk_to: None,
        via_rel: None,
        adapter: false,
    };

    let storage = to_storage(vec![create_test_struct_metadata(
        "UserInArticle",
        "struct UserInArticle { pub id: i32 }",
    )]);
    let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
    let adapter_name = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());

    let error = maybe_generate_same_file_relation_override(
        &new_type_name,
        "user",
        &adapter_name,
        &rel_info,
        &storage,
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("UserInArticle"));
    assert!(error.to_string().contains("related model type"));
}

#[test]
fn related_model_type_renames_only_final_segment() {
    // A module segment that itself contains "Schema" (capital S) must be
    // left verbatim — only the trailing `…Schema` ident becomes `…Model`.
    // The previous `to_string().replace("Schema","Model")` corrupted
    // `SchemaStore` into `ModelStore`; this locks the regression.
    let ty = related_model_type_from_schema_path(&quote!(crate::SchemaStore::user::UserSchema))
        .expect("valid schema path resolves to a model type");
    assert_eq!(
        quote!(#ty).to_string(),
        quote!(crate::SchemaStore::user::UserModel).to_string(),
    );
    // A bare trailing `Schema` ident maps to `Model`.
    let ty2 = related_model_type_from_schema_path(&quote!(crate::models::user::Schema))
        .expect("valid schema path");
    assert_eq!(
        quote!(#ty2).to_string(),
        quote!(crate::models::user::Model).to_string(),
    );
    // Only the suffix is renamed: an inner `Schema` of the ident stays.
    let ty3 = related_model_type_from_schema_path(&quote!(crate::FooSchemaBarSchema))
        .expect("valid schema path");
    assert_eq!(
        quote!(#ty3).to_string(),
        quote!(crate::FooSchemaBarModel).to_string(),
    );
    // No `Schema` suffix: the historical rename of every occurrence.
    let ty4 = related_model_type_from_schema_path(&quote!(crate::SchemaUser)).expect("valid path");
    assert_eq!(
        quote!(#ty4).to_string(),
        quote!(crate::ModelUser).to_string()
    );
    // A non-path token stream (e.g. a stray `?`) yields None, not a panic.
    assert!(related_model_type_from_schema_path(&quote!(?)).is_none());
}

#[test]
fn test_generate_schema_type_code_normal_mode_relation_rename_and_custom_name() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "articles")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub owner: HasOne<super::user::Entity>
            }"#,
    )]);

    let tokens = quote!(
        ArticleResponse from Model,
        name = "CustomArticleSchema",
        rename = [("name", "display_name")]
    );
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("display_name"));
    assert!(output.contains("owner"));
    assert!(output.contains("Clone"));
    assert!(output.contains("CustomArticleSchema"));
    assert_eq!(metadata.unwrap().name, "CustomArticleSchema");
}

#[test]
fn override_ref_honors_dto_schema_name_attribute() {
    // When the adapter DTO overrides its OpenAPI component name via
    // `#[schema(name = "...")]`, the generated wrapper's `#[schema(ref = ...)]`
    // must use that name (not the Rust struct name) so the emitted `$ref`
    // resolves instead of dangling.
    let rel_info = RelationFieldInfo {
        field_name: syn::Ident::new("user", proc_macro2::Span::call_site()),
        relation_type: "HasOne".to_string(),
        schema_path: quote!(crate::models::user::Schema),
        is_optional: true,
        inline_type_info: None,
        relation_enum: None,
        fk_column: None,
        fk_optional: true,
        fk_to: None,
        via_rel: None,
        adapter: false,
    };
    let storage = to_storage(vec![create_test_struct_metadata(
        "UserInArticle",
        r#"#[schema(name = "ArticleUser")] struct UserInArticle { pub id: i32, pub name: String }"#,
    )]);
    let new_type_name = syn::Ident::new("ArticleResponse", proc_macro2::Span::call_site());
    let adapter_name = syn::Ident::new("UserInArticle", proc_macro2::Span::call_site());

    let (_field_ty, helpers) = maybe_generate_same_file_relation_override(
        &new_type_name,
        "user",
        &adapter_name,
        &rel_info,
        &storage,
        true,
    )
    .expect("override generation should succeed");

    let output = helpers.to_string();
    assert!(
        output.contains("ref = \"ArticleUser\""),
        "wrapper $ref must use the DTO's #[schema(name=...)] override, got: {output}"
    );
    assert!(
        !output.contains("ref = \"UserInArticle\""),
        "must not fall back to the struct name when a name override exists, got: {output}"
    );
}

/// R35-SM4-L1: a container `cfg_attr` serde default is judged only while its
/// predicate is statically active, the one the proxy honours; an inactive one
/// names nothing the call site must resolve.
#[test]
fn container_cfg_attr_serde_is_judged_only_while_active() {
    let inactive: syn::ItemStruct = syn::parse_str(
        "#[cfg_attr(any(), serde(default = \"nowhere_fn\"))] struct Dto { id: i32 }",
    )
    .unwrap();
    assert_eq!(unresolved::unresolved_at_call_site(&inactive, None), None);
    let active: syn::ItemStruct = syn::parse_str(
        "#[cfg_attr(all(), serde(default = \"nowhere_fn\"))] struct Dto { id: i32 }",
    )
    .unwrap();
    assert_eq!(
        unresolved::unresolved_at_call_site(&active, None).as_deref(),
        Some("nowhere_fn")
    );
    // A container `cfg_attr` without its comma is not a predicate: nothing.
    let malformed: syn::ItemStruct =
        syn::parse_str("#[cfg_attr(unix)] struct Dto { id: i32 }").unwrap();
    assert_eq!(unresolved::unresolved_at_call_site(&malformed, None), None);
}
