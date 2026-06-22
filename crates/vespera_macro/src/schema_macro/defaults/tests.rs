use std::collections::HashMap;

use super::*;
use crate::metadata::StructMetadata;
use crate::schema_macro::{SchemaTypeInput, generate_schema_type_code};

fn create_test_struct_metadata(name: &str, definition: &str) -> StructMetadata {
    StructMetadata::new(name.to_string(), definition.to_string())
}

fn to_storage(items: Vec<StructMetadata>) -> HashMap<String, StructMetadata> {
    items.into_iter().map(|s| (s.name.clone(), s)).collect()
}

// ======================================
// validate_literal_default tests
// ======================================

#[test]
fn validate_literal_default_accepts_valid_primitives() {
    let i32_ty: syn::Type = syn::parse_str("i32").unwrap();
    assert!(validate_literal_default("42", &i32_ty).is_ok());
    let u8_ty: syn::Type = syn::parse_str("u8").unwrap();
    assert!(validate_literal_default("255", &u8_ty).is_ok());
    let f64_ty: syn::Type = syn::parse_str("f64").unwrap();
    assert!(validate_literal_default("0.7", &f64_ty).is_ok());
    let bool_ty: syn::Type = syn::parse_str("bool").unwrap();
    assert!(validate_literal_default("true", &bool_ty).is_ok());
    // String FromStr is infallible; Decimal is intentionally left to runtime.
    let string_ty: syn::Type = syn::parse_str("String").unwrap();
    assert!(validate_literal_default("anything at all", &string_ty).is_ok());
    let decimal_ty: syn::Type = syn::parse_str("Decimal").unwrap();
    assert!(validate_literal_default("not-validated-here", &decimal_ty).is_ok());
}

#[test]
fn validate_literal_default_rejects_unparseable_and_out_of_range() {
    let i32_ty: syn::Type = syn::parse_str("i32").unwrap();
    assert!(validate_literal_default("abc", &i32_ty).is_err());
    // Range violation caught against the EXACT type, not a generic integer.
    let u8_ty: syn::Type = syn::parse_str("u8").unwrap();
    assert!(validate_literal_default("300", &u8_ty).is_err());
    let bool_ty: syn::Type = syn::parse_str("bool").unwrap();
    assert!(validate_literal_default("maybe", &bool_ty).is_err());
    let f64_ty: syn::Type = syn::parse_str("f64").unwrap();
    assert!(validate_literal_default("3.14.15", &f64_ty).is_err());
}

// ======================================
// generate_sea_orm_default_attrs tests
// ======================================

#[test]
fn test_sea_orm_default_attrs_valid_literal_keeps_parse_unwrap() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "42")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("i32").unwrap();
    let mut fns = Vec::new();
    let (serde, _schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "count", &ty, &ty, false, &mut fns);
    assert!(serde.to_string().contains("serde"));
    assert_eq!(fns.len(), 1);
    let body = fns[0].to_string();
    assert!(body.contains("parse"), "valid literal keeps parse: {body}");
    assert!(
        body.contains("unwrap"),
        "valid literal keeps unwrap: {body}"
    );
    assert!(
        !body.contains("compile_error"),
        "valid literal must not emit compile_error: {body}"
    );
}

#[test]
fn test_sea_orm_default_attrs_invalid_literal_emits_compile_error() {
    // `"abc"` cannot parse to i32: the generated default function body must
    // be a compile_error (pointing at the field) instead of a runtime
    // `.parse().unwrap()` that would panic when serde fills a missing field.
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "abc")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("i32").unwrap();
    let mut fns = Vec::new();
    let (serde, _schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "count", &ty, &ty, false, &mut fns);
    assert!(serde.to_string().contains("serde"));
    assert_eq!(fns.len(), 1);
    let body = fns[0].to_string();
    assert!(
        body.contains("compile_error"),
        "invalid literal must emit compile_error: {body}"
    );
    assert!(
        !body.contains("unwrap"),
        "invalid literal must not emit a runtime parse().unwrap(): {body}"
    );
}

#[test]
fn test_sea_orm_default_attrs_optional_field_skips() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "42")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("i32").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "count", &ty, &ty, true, &mut fns);
    assert!(serde.is_empty());
    assert!(schema.is_empty());
    assert!(fns.is_empty());
}

#[test]
fn test_sea_orm_default_attrs_no_default_and_no_pk() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(unique)])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("String").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "email", &ty, &ty, false, &mut fns);
    assert!(serde.is_empty());
    assert!(schema.is_empty());
    assert!(fns.is_empty());
}

#[test]
fn test_sea_orm_default_attrs_primary_key_generates_defaults() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(primary_key)])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("i32").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "id", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "primary_key should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains('0'),
        "primary_key i32 should have schema default 0: {schema_str}"
    );
    assert_eq!(fns.len(), 1, "should generate a default function");
}

#[test]
fn test_sea_orm_default_attrs_sql_function_generates_defaults() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("DateTimeWithTimeZone").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "created_at",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "SQL function default should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("1970-01-01"),
        "DateTimeWithTimeZone should have epoch default: {schema_str}"
    );
    assert_eq!(fns.len(), 1, "should generate a default function");
}

#[test]
fn test_sea_orm_default_attrs_sql_function_uuid() {
    let attrs: Vec<syn::Attribute> =
        vec![syn::parse_quote!(#[sea_orm(primary_key, default_value = "gen_random_uuid()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("Uuid").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "id", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "UUID SQL default should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("00000000-0000-0000-0000-000000000000"),
        "Uuid should have nil UUID default: {schema_str}"
    );
    assert_eq!(fns.len(), 1);
}

#[test]
fn test_sea_orm_default_attrs_sql_function_unknown_type_skips() {
    let attrs: Vec<syn::Attribute> =
        vec![syn::parse_quote!(#[sea_orm(default_value = "SOME_FUNC()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("MyCustomType").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "field", &ty, &ty, false, &mut fns);
    assert!(serde.is_empty(), "unknown type should skip serde default");
    assert!(schema.is_empty(), "unknown type should skip schema default");
    assert!(fns.is_empty());
}

#[test]
fn test_sea_orm_default_attrs_existing_serde_default() {
    let attrs: Vec<syn::Attribute> = vec![
        syn::parse_quote!(#[sea_orm(default_value = "42")]),
        syn::parse_quote!(#[serde(default)]),
    ];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("i32").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "count", &ty, &ty, false, &mut fns);
    // serde attr should be empty (already has serde default)
    assert!(serde.is_empty());
    // schema attr should still be generated
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("schema"),
        "should have schema attr: {schema_str}"
    );
    assert!(
        fns.is_empty(),
        "no default fn needed when serde(default) exists"
    );
}

#[test]
fn test_sea_orm_default_attrs_non_parseable_type() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "Active")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("MyEnum").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "status", &ty, &ty, false, &mut fns);
    // serde attr empty (non-parseable type)
    assert!(serde.is_empty());
    // schema attr still generated
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("schema"),
        "should have schema attr: {schema_str}"
    );
    assert!(fns.is_empty());
}

#[test]
fn test_sea_orm_default_attrs_full_generation() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "42")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("i32").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "count", &ty, &ty, false, &mut fns);
    // Both serde and schema attrs should be generated
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde attr: {serde_str}"
    );
    assert!(
        serde_str.contains("default_Test_count"),
        "should reference generated fn: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("schema"),
        "should have schema attr: {schema_str}"
    );
    // Default function should be generated
    assert_eq!(fns.len(), 1, "should generate one default function");
    let fn_str = fns[0].to_string();
    assert!(
        fn_str.contains("default_Test_count"),
        "fn name should match: {fn_str}"
    );
}

#[test]
fn test_generate_schema_type_code_with_partial_all() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String, pub bio: Option<String> }",
    )]);

    let tokens = quote!(UpdateUser from User, partial);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("Option < i32 >"));
    assert!(output.contains("Option < String >"));
}

#[test]
fn test_generate_schema_type_code_with_partial_fields() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String, pub email: String }",
    )]);

    let tokens = quote!(UpdateUser from User, partial = ["name"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(
        output.contains("UpdateUser"),
        "should contain generated struct name: {output}"
    );
}

// ============================================================
// Coverage: omit_default in generate_schema_type_code (line 180)
// ============================================================

#[test]
fn test_generate_schema_type_code_with_omit_default() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "items")]
            pub struct Model {
                #[sea_orm(primary_key)]
                pub id: i32,
                pub name: String,
                #[sea_orm(default_value = "NOW()")]
                pub created_at: DateTimeWithTimeZone,
            }"#,
    )]);

    let tokens = quote!(CreateItemRequest from Model, omit_default);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // id (primary_key) and created_at (default_value) should be omitted
    assert!(
        !output.contains("id :"),
        "id should be omitted by omit_default: {output}"
    );
    assert!(
        !output.contains("created_at"),
        "created_at should be omitted by omit_default: {output}"
    );
    // name should remain
    assert!(output.contains("name"), "name should remain: {output}");
}

// ============================================================
// Coverage: SQL function default with existing serde default (line 554)
// ============================================================

#[test]
fn test_sea_orm_default_attrs_sql_function_with_existing_serde_default() {
    let attrs: Vec<syn::Attribute> = vec![
        syn::parse_quote!(#[sea_orm(default_value = "NOW()")]),
        syn::parse_quote!(#[serde(default)]),
    ];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("DateTimeWithTimeZone").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "created_at",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    // serde attr should be empty (already has serde default)
    assert!(serde.is_empty());
    // schema attr should still be generated
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("schema"),
        "should have schema attr: {schema_str}"
    );
    assert!(
        schema_str.contains("1970-01-01"),
        "should have epoch default: {schema_str}"
    );
    assert!(
        fns.is_empty(),
        "no default fn needed when serde(default) exists"
    );
}

// ============================================================
// Coverage: sql_function_default_for_type branches (lines 580-615)
// ============================================================

#[test]
fn test_sea_orm_default_attrs_sql_function_non_path_type() {
    // Non-Path type (reference) triggers early return None in sql_function_default_for_type
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("&str").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "field", &ty, &ty, false, &mut fns);
    assert!(serde.is_empty(), "non-Path type should skip serde default");
    assert!(
        schema.is_empty(),
        "non-Path type should skip schema default"
    );
    assert!(fns.is_empty());
}

#[test]
fn test_sea_orm_default_attrs_sql_function_datetime() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("DateTime").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "created_at",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "DateTime should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("1970-01-01T00:00:00+00:00"),
        "DateTime should have epoch default: {schema_str}"
    );
    assert_eq!(fns.len(), 1);
}

#[test]
fn test_sea_orm_default_attrs_sql_function_naive_datetime() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("NaiveDateTime").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "created_at",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "NaiveDateTime should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("1970-01-01T00:00:00"),
        "NaiveDateTime should have epoch default: {schema_str}"
    );
    assert_eq!(fns.len(), 1);
}

#[test]
fn test_sea_orm_default_attrs_sql_function_naive_date() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("NaiveDate").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "date_field",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "NaiveDate should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("1970-01-01"),
        "NaiveDate should have date default: {schema_str}"
    );
    assert_eq!(fns.len(), 1);
}

#[test]
fn test_sea_orm_default_attrs_sql_function_naive_time() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("NaiveTime").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "time_field",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "NaiveTime should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("00:00:00"),
        "NaiveTime should have time default: {schema_str}"
    );
    assert_eq!(fns.len(), 1);
}

#[test]
fn test_sea_orm_default_attrs_sql_function_time_type() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("Time").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "time_field",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "Time should generate serde default: {serde_str}"
    );
    let schema_str = schema.to_string();
    assert!(
        schema_str.contains("00:00:00"),
        "Time should have time default: {schema_str}"
    );
    assert_eq!(fns.len(), 1);
}

// --- Coverage: is_parseable_type empty segments ---

#[test]
fn test_is_parseable_type_empty_segments() {
    // Synthetically construct a Type::Path with empty segments (impossible through parsing)
    let ty = syn::Type::Path(syn::TypePath {
        qself: None,
        path: syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::new(),
        },
    });
    assert!(!is_parseable_type(&ty));
}

#[test]
fn test_generate_schema_type_code_partial_nonexistent_field() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UpdateUser from User, partial = ["nonexistent"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("does not exist"));
    assert!(err.contains("nonexistent"));
}

#[test]
fn test_generate_schema_type_code_partial_from_impl_wraps_some() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UpdateUser from User, partial);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("Some (source . id)"));
    assert!(output.contains("Some (source . name)"));
}

#[test]
fn test_generate_schema_type_code_preserves_struct_doc() {
    let input = SchemaTypeInput {
        new_type: syn::Ident::new("NewUser", proc_macro2::Span::call_site()),
        source_type: syn::parse_str("User").unwrap(),
        omit: None,
        pick: None,
        rename: None,
        add: None,
        derive_clone: true,
        partial: None,
        schema_name: None,
        ignore_schema: false,
        rename_all: None,
        multipart: false,
        omit_default: false,
    };
    let struct_def = StructMetadata {
        name: "User".to_string(),
        definition: r"
                /// User struct documentation
                pub struct User {
                    /// The user ID
                    pub id: i32,
                    /// The user name
                    pub name: String,
                }
            "
        .to_string(),
        include_in_openapi: true,
        field_defaults: std::collections::BTreeMap::new(),
        source_identity: None,
    };
    let storage = to_storage(vec![struct_def]);
    let result = generate_schema_type_code(&input, &storage);
    assert!(result.is_ok());
    let (tokens, _) = result.unwrap();
    let tokens_str = tokens.to_string();
    assert!(tokens_str.contains("User struct documentation") || tokens_str.contains("doc"));
}

// Tests for serde attribute filtering from source struct

#[test]
fn test_generate_schema_type_code_inherits_source_rename_all() {
    // Source struct has serde(rename_all = "snake_case")
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        r#"#[serde(rename_all = "snake_case")]
            pub struct User { pub id: i32, pub user_name: String }"#,
    )]);

    let tokens = quote!(UserResponse from User);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should use snake_case from source
    assert!(output.contains("rename_all"));
    assert!(output.contains("snake_case"));
}

#[test]
fn test_generate_schema_type_code_override_rename_all() {
    // Source has snake_case, but we override with camelCase
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        r#"#[serde(rename_all = "snake_case")]
            pub struct User { pub id: i32, pub user_name: String }"#,
    )]);

    let tokens = quote!(UserResponse from User, rename_all = "camelCase");
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should use camelCase (our override)
    assert!(output.contains("camelCase"));
}
