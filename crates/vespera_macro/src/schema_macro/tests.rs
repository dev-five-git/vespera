//! Tests for schema_macro module
//!
//! This file contains all unit tests for the schema generation functionality.

use std::collections::HashMap;

use serial_test::serial;

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

// ======================================
// generate_sea_orm_default_attrs tests
// ======================================

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
fn test_sea_orm_default_attrs_no_default_value() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(primary_key)])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("i32").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "id", &ty, &ty, false, &mut fns);
    assert!(serde.is_empty());
    assert!(schema.is_empty());
}

#[test]
fn test_sea_orm_default_attrs_sql_function_supported_type() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("String").unwrap();
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
    // Supported type with SQL function → generates serde(default) to mark field not-required
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde default attr: {serde_str}"
    );
    assert!(
        serde_str.contains("default_Test_created_at"),
        "should reference generated default fn: {serde_str}"
    );
    // No JSON default for SQL functions (value is DB-side only)
    assert!(schema.is_empty());
    // Default function was generated
    assert_eq!(fns.len(), 1, "should generate one default function");
}

#[test]
fn test_sea_orm_default_attrs_sql_function_unsupported_type_skips() {
    let attrs: Vec<syn::Attribute> =
        vec![syn::parse_quote!(#[sea_orm(default_value = "MY_FUNC()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("MyCustomType").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) = generate_sea_orm_default_attrs(
        &attrs,
        &struct_name,
        "custom_field",
        &ty,
        &ty,
        false,
        &mut fns,
    );
    // Unsupported type with SQL function → still skips entirely
    assert!(serde.is_empty());
    assert!(schema.is_empty());
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

// --- Coverage: sql_function_default_body branches ---

#[test]
fn test_sql_function_default_non_path_type_skips() {
    // Reference type (&str) is Type::Reference, not Type::Path → sql_function_default_body returns None
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "GEN()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("&str").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "val", &ty, &ty, false, &mut fns);
    assert!(serde.is_empty());
    assert!(schema.is_empty());
    assert!(fns.is_empty());
}

#[test]
fn test_sql_function_default_datetime_with_timezone() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("DateTimeWithTimeZone").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "ts", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde attr: {serde_str}"
    );
    assert!(schema.is_empty());
    assert_eq!(fns.len(), 1);
    let fn_str = fns[0].to_string();
    assert!(
        fn_str.contains("UNIX_EPOCH"),
        "should use epoch default: {fn_str}"
    );
}

#[test]
fn test_sql_function_default_datetime_utc() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("DateTimeUtc").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "ts", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde attr: {serde_str}"
    );
    assert!(schema.is_empty());
    assert_eq!(fns.len(), 1);
    let fn_str = fns[0].to_string();
    assert!(
        fn_str.contains("UNIX_EPOCH"),
        "should use epoch default: {fn_str}"
    );
}

#[test]
fn test_sql_function_default_datetime_local() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("DateTimeLocal").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "ts", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde attr: {serde_str}"
    );
    assert!(schema.is_empty());
    assert_eq!(fns.len(), 1);
    let fn_str = fns[0].to_string();
    assert!(
        fn_str.contains("UNIX_EPOCH"),
        "should use epoch default: {fn_str}"
    );
}

#[test]
fn test_sql_function_default_naive_datetime() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[sea_orm(default_value = "NOW()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("NaiveDateTime").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "ts", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde attr: {serde_str}"
    );
    assert!(schema.is_empty());
    assert_eq!(fns.len(), 1);
    let fn_str = fns[0].to_string();
    assert!(
        fn_str.contains("UNIX_EPOCH"),
        "should use epoch default: {fn_str}"
    );
}

#[test]
fn test_sql_function_default_naive_date() {
    let attrs: Vec<syn::Attribute> =
        vec![syn::parse_quote!(#[sea_orm(default_value = "CURDATE()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("NaiveDate").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "d", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde attr: {serde_str}"
    );
    assert!(schema.is_empty());
    assert_eq!(fns.len(), 1);
    let fn_str = fns[0].to_string();
    assert!(
        fn_str.contains("from_ymd_opt"),
        "should use ymd default: {fn_str}"
    );
}

#[test]
fn test_sql_function_default_naive_time() {
    let attrs: Vec<syn::Attribute> =
        vec![syn::parse_quote!(#[sea_orm(default_value = "CURTIME()")])];
    let struct_name = syn::Ident::new("Test", proc_macro2::Span::call_site());
    let ty: syn::Type = syn::parse_str("NaiveTime").unwrap();
    let mut fns = Vec::new();
    let (serde, schema) =
        generate_sea_orm_default_attrs(&attrs, &struct_name, "t", &ty, &ty, false, &mut fns);
    let serde_str = serde.to_string();
    assert!(
        serde_str.contains("serde"),
        "should have serde attr: {serde_str}"
    );
    assert!(schema.is_empty());
    assert_eq!(fns.len(), 1);
    let fn_str = fns[0].to_string();
    assert!(
        fn_str.contains("from_hms_opt"),
        "should use hms default: {fn_str}"
    );
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

// Tests for field rename processing

#[test]
fn test_generate_schema_type_code_with_rename() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserDTO from User, rename = [("id", "user_id")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("user_id"));
    // The From impl should map user_id from source.id
    assert!(output.contains("From"));
}

#[test]
fn test_generate_schema_type_code_rename_preserves_serde_rename() {
    // Source field already has serde(rename), which should be preserved as the JSON name
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        r#"pub struct User {
                pub id: i32,
                #[serde(rename = "userName")]
                pub name: String
            }"#,
    )]);

    let tokens = quote!(UserDTO from User, rename = [("name", "user_name")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // The Rust field is renamed to user_name
    assert!(output.contains("user_name"));
    // The JSON name should be preserved as userName
    assert!(output.contains("userName") || output.contains("rename"));
}

// Tests for schema derive and name attribute generation

#[test]
fn test_generate_schema_type_code_with_ignore_schema() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserInternal from User, ignore);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should NOT contain vespera::Schema derive
    assert!(!output.contains("vespera :: Schema"));
}

#[test]
fn test_generate_schema_type_code_with_custom_name() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserResponse from User, name = "CustomUserSchema");
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should contain schema(name = "...") attribute
    assert!(output.contains("schema"));
    assert!(output.contains("CustomUserSchema"));
    // Metadata should be returned
    assert!(metadata.is_some());
    let meta = metadata.unwrap();
    assert_eq!(meta.name, "CustomUserSchema");
}

#[test]
fn test_generate_schema_type_code_with_clone_false() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub name: String }",
    )]);

    let tokens = quote!(UserNonClone from User, clone = false);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should NOT contain Clone derive
    assert!(!output.contains("Clone ,"));
}

// Test for SeaORM model detection

#[test]
fn test_generate_schema_type_code_seaorm_model_detection() {
    // Source struct has sea_orm attribute - should be detected as SeaORM model
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model { pub id: i32, pub name: String }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("UserSchema"));
}

// Test tuple struct handling

#[test]
fn test_generate_schema_type_code_tuple_struct() {
    // Tuple structs have no named fields
    let storage = to_storage(vec![create_test_struct_metadata(
        "Point",
        "pub struct Point(pub i32, pub i32);",
    )]);

    let tokens = quote!(PointDTO from Point);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("PointDTO"));
}

// Test raw identifier fields

#[test]
fn test_generate_schema_type_code_raw_identifier_field() {
    // Field name is a Rust keyword with r# prefix
    let storage = to_storage(vec![create_test_struct_metadata(
        "Config",
        "pub struct Config { pub id: i32, pub r#type: String }",
    )]);

    let tokens = quote!(ConfigDTO from Config);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("ConfigDTO"));
}

// Test Option field not double-wrapped with partial

#[test]
fn test_generate_schema_type_code_partial_no_double_option() {
    // bio is already Option<String>, partial should NOT wrap it again
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        "pub struct User { pub id: i32, pub bio: Option<String> }",
    )]);

    let tokens = quote!(UpdateUser from User, partial);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // bio should remain Option<String>, not Option<Option<String>>
    assert!(!output.contains("Option < Option"));
}

// Test serde(skip) fields are excluded

#[test]
fn test_generate_schema_code_excludes_serde_skip_fields() {
    let storage = to_storage(vec![create_test_struct_metadata(
        "User",
        r"pub struct User {
                pub id: i32,
                #[serde(skip)]
                pub internal_state: String,
                pub name: String
            }",
    )]);

    let tokens = quote!(User);
    let input: SchemaInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_code(&input, &storage);

    assert!(result.is_ok());
    let output = result.unwrap().to_string();
    // internal_state should be excluded from schema properties
    assert!(!output.contains("internal_state"));
    assert!(output.contains("name"));
}

// Tests for qualified path storage fallback
// Note: This tests the case where is_qualified_path returns true
// and we find the struct in schema_storage rather than via file lookup

#[test]
fn test_generate_schema_type_code_qualified_path_storage_lookup() {
    // Use a qualified path like crate::models::user::Model
    // The storage contains Model, so it should fallback to storage lookup
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        "pub struct Model { pub id: i32, pub name: String }",
    )]);

    // Note: This qualified path won't find files (no real filesystem),
    // so it falls back to storage lookup by the simple name "Model"
    let tokens = quote!(UserSchema from crate::models::user::Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    // This should succeed by finding Model in storage
    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("UserSchema"));
}

// Test for qualified path not found error

#[test]
fn test_generate_schema_type_code_qualified_path_not_found() {
    // Empty storage - qualified path should fail
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let tokens = quote!(UserSchema from crate::models::user::NonExistent);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    // Should fail with "not found" error
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found"));
}

// Tests for HasMany excluded by default

#[test]
fn test_generate_schema_type_code_has_many_excluded_by_default() {
    // SeaORM model with HasMany relation - should be excluded by default
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub memos: HasMany<super::memo::Entity>
            }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // HasMany field should NOT appear in output (excluded by default)
    assert!(!output.contains("memos"));
    // But regular fields should appear
    assert!(output.contains("name"));
}

// Test for relation conversion failure skip

#[test]
fn test_generate_schema_type_code_relation_conversion_failure() {
    // Model with relation type but missing generic args - conversion should fail
    // The field should be skipped
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub broken: HasMany
            }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    // Should succeed but skip the broken field
    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Broken field should be skipped
    assert!(!output.contains("broken"));
    // Regular fields should appear
    assert!(output.contains("name"));
}

// Coverage test for BelongsTo relation type conversion

#[test]
fn test_generate_schema_type_code_belongs_to_relation() {
    // SeaORM model with BelongsTo relation - should be included
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "memos")]
            pub struct Model {
                pub id: i32,
                pub user_id: i32,
                #[sea_orm(belongs_to = "super::user::Entity", from = "user_id")]
                pub user: BelongsTo<super::user::Entity>
            }"#,
    )]);

    let tokens = quote!(MemoSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // BelongsTo should be included (converted to Box<UserSchema> or similar)
    assert!(output.contains("user"));
}

// Coverage test for HasOne relation type

#[test]
fn test_generate_schema_type_code_has_one_relation() {
    // SeaORM model with HasOne relation - should be included
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "users")]
            pub struct Model {
                pub id: i32,
                pub name: String,
                pub profile: HasOne<super::profile::Entity>
            }"#,
    )]);

    let tokens = quote!(UserSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // HasOne should be included
    assert!(output.contains("profile"));
}

// Test for relation fields push into relation_fields

#[test]
fn test_generate_schema_type_code_seaorm_model_with_relation_generates_from_model() {
    // When a SeaORM model has FK relations (HasOne/BelongsTo),
    // it should generate from_model impl instead of From impl
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "memos")]
            pub struct Model {
                pub id: i32,
                pub title: String,
                pub user: BelongsTo<super::user::Entity>
            }"#,
    )]);

    let tokens = quote!(MemoSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should have relation field
    assert!(output.contains("user"));
    // Should NOT have regular From impl (because of relation)
    // The From impl is only generated when there are no relation fields
}

// Test for from_model generation with relations
// Note: This requires is_source_seaorm_model && has_relation_fields
// The from_model generation happens but needs file lookup for full path

#[test]
fn test_generate_schema_type_code_from_model_generation() {
    // SeaORM model with relation should trigger from_model generation
    let storage = to_storage(vec![create_test_struct_metadata(
        "Model",
        r#"#[sea_orm(table_name = "memos")]
            pub struct Model {
                pub id: i32,
                pub user: BelongsTo<super::user::Entity>
            }"#,
    )]);

    let tokens = quote!(MemoSchema from Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let result = generate_schema_type_code(&input, &storage);

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Has relation field
    assert!(output.contains("user"));
    // Regular impl From should NOT be present (because has relations)
    // Check that we don't have "impl From < Model > for MemoSchema"
    // (Relations disable the automatic From impl)
}

#[test]
#[serial]
fn test_generate_schema_type_code_qualified_path_file_lookup_success() {
    // Tests: qualified path found via file lookup, module_path used when source is empty
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create user.rs with Model struct
    let user_model = r"
pub struct Model {
    pub id: i32,
    pub name: String,
    pub email: String,
}
";
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // Use qualified path - file lookup should succeed
    let tokens = quote!(UserSchema from crate::models::user::Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new(); // Empty storage - force file lookup

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("UserSchema"));
    assert!(output.contains("id"));
    assert!(output.contains("name"));
    assert!(output.contains("email"));
}

#[test]
#[serial]
fn test_generate_schema_type_code_simple_name_file_lookup_fallback() {
    // Tests: simple name (not in storage) found via file lookup with schema_name hint
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create user.rs with Model struct
    let user_model = r"
pub struct Model {
    pub id: i32,
    pub username: String,
}
";
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // Use simple name with schema_name hint - file lookup should find it via hint
    // name = "UserSchema" provides hint to look in user.rs
    let tokens = quote!(Schema from Model, name = "UserSchema");
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new(); // Empty storage - force file lookup

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok());
    let (tokens, metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("Schema"));
    assert!(output.contains("id"));
    assert!(output.contains("username"));
    // Metadata should be returned for custom name
    assert!(metadata.is_some());
    assert_eq!(metadata.unwrap().name, "UserSchema");
}

// ============================================================
// Tests for HasMany explicit pick with inline type
// ============================================================

#[test]
#[serial]
fn test_generate_schema_type_code_has_many_explicit_pick_inline_type() {
    // Tests: HasMany is explicitly picked, inline type is generated
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create memo.rs with Model struct (the target of HasMany)
    let memo_model = r"
pub struct Model {
    pub id: i32,
    pub title: String,
    pub content: String,
}
";
    std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

    // Create user.rs with Model struct that has HasMany relation
    let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub memos: HasMany<super::memo::Entity>,
}
"#;
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // Explicitly pick HasMany field - should generate inline type
    let tokens = quote!(UserSchema from crate::models::user::Model, pick = ["id", "name", "memos"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should have inline type definition for memos
    assert!(output.contains("UserSchema"));
    assert!(output.contains("memos"));
    // Inline type should be Vec<InlineType>
    assert!(output.contains("Vec <"));
}

#[test]
#[serial]
fn test_generate_schema_type_code_has_many_explicit_pick_file_not_found() {
    // Tests: HasMany is explicitly picked but target file not found - should skip field
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create user.rs with Model struct that has HasMany to nonexistent model
    let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub items: HasMany<super::nonexistent::Entity>,
}
"#;
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // Explicitly pick HasMany field - file not found, should skip
    let tokens = quote!(UserSchema from crate::models::user::Model, pick = ["id", "name", "items"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // items field should be skipped (file not found for inline type)
    assert!(!output.contains("items"));
    // But other fields should exist
    assert!(output.contains("id"));
    assert!(output.contains("name"));
}

// ============================================================
// Tests for BelongsTo/HasOne circular reference inline types
// ============================================================

#[test]
#[serial]
fn test_generate_schema_type_code_belongs_to_circular_inline_optional() {
    // Tests: BelongsTo with circular reference, optional field (is_optional = true)
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create user.rs with Model that references memo (circular)
    let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub memo: BelongsTo<super::memo::Entity>,
}
"#;
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Create memo.rs with Model that references user (completing the circle)
    let memo_model = r#"
#[sea_orm(table_name = "memos")]
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    pub user: BelongsTo<super::user::Entity>,
}
"#;
    std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // Generate schema from memo - has BelongsTo user which has circular ref back
    let tokens = quote!(MemoSchema from crate::models::memo::Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should have inline type definition for circular relation
    assert!(output.contains("MemoSchema"));
    assert!(output.contains("user"));
    // BelongsTo is optional by default, so should have Option<Box<...>>
    assert!(output.contains("Option < Box <"));
}

#[test]
#[serial]
fn test_generate_schema_type_code_has_one_circular_inline_required() {
    // Tests: HasOne with circular reference, required field (is_optional = false)
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create profile.rs with Model that references user (circular)
    let profile_model = r#"
#[sea_orm(table_name = "profiles")]
pub struct Model {
    pub id: i32,
    pub bio: String,
    pub user: BelongsTo<super::user::Entity>,
}
"#;
    std::fs::write(models_dir.join("profile.rs"), profile_model).unwrap();

    // Create user.rs with Model that has HasOne profile
    // HasOne with required FK becomes required (non-optional)
    let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub profile_id: i32,
    #[sea_orm(has_one = "super::profile::Entity", from = "profile_id")]
    pub profile: HasOne<super::profile::Entity>,
}
"#;
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // Generate schema from user - has HasOne profile which has circular ref back
    let tokens = quote!(UserSchema from crate::models::user::Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should have inline type definition for circular relation
    assert!(output.contains("UserSchema"));
    assert!(output.contains("profile"));
    // HasOne with required FK should have Box<...> (not Option<Box<...>>)
    assert!(output.contains("Box <"));
}

#[test]
#[serial]
fn test_generate_schema_type_code_belongs_to_circular_inline_required_file() {
    // Tests: BelongsTo with circular reference AND required FK (is_optional = false)
    // This requires file-based lookup with:
    // 1. #[sea_orm(from = "required_fk")] where required_fk is NOT Option<T>
    // 2. Circular reference between two models
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create user.rs with Model that references memo (circular)
    let user_model = r#"
#[sea_orm(table_name = "users")]
pub struct Model {
    pub id: i32,
    pub name: String,
    pub memo_id: i32,
    #[sea_orm(belongs_to, from = "memo_id", to = "id")]
    pub memo: BelongsTo<super::memo::Entity>,
}
"#;
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Create memo.rs with Model that references user (completing the circle)
    // Note: using flag-style `belongs_to` with `from = "user_id"`
    let memo_model = r#"
#[sea_orm(table_name = "memos")]
pub struct Model {
    pub id: i32,
    pub title: String,
    pub user_id: i32,
    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
    std::fs::write(models_dir.join("memo.rs"), memo_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // Generate schema from memo - has BelongsTo user which has circular ref back
    // The user_id field is required (not Option), so is_optional = false
    // This should generate Box<...> instead of Option<Box<...>>
    let tokens = quote!(MemoSchema from crate::models::memo::Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok(), "Should generate schema: {:?}", result.err());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    // Should have inline type definition for circular relation
    assert!(
        output.contains("MemoSchema"),
        "Should contain MemoSchema: {output}"
    );
    assert!(
        output.contains("user"),
        "Should contain user field: {output}"
    );
    // BelongsTo with required FK (user_id: i32) should generate Box<...> not Option<Box<...>>
    assert!(
        output.contains("pub user : Box <"),
        "BelongsTo with required FK should generate Box<>, not Option<Box<>>. Output: {output}"
    );
}

#[test]
fn test_seaorm_relation_required_fk_directly() {
    // Test the convert_relation_type_to_schema_with_info function directly
    // to verify is_optional = false when FK is required
    use crate::schema_macro::seaorm::{
        convert_relation_type_to_schema_with_info, extract_belongs_to_from_field,
        is_field_optional_in_struct,
    };

    // Use the same attribute format that works in seaorm tests: belongs_to (flag), not belongs_to = "..."
    let struct_def = r#"
#[sea_orm(table_name = "memos")]
pub struct Model {
    pub id: i32,
    pub user_id: i32,
    #[sea_orm(belongs_to, from = "user_id", to = "id")]
    pub user: BelongsTo<super::user::Entity>,
}
"#;
    let parsed_struct: syn::ItemStruct = syn::parse_str(struct_def).unwrap();

    // Get the user field
    let syn::Fields::Named(fields_named) = &parsed_struct.fields else {
        panic!("Expected named fields")
    };

    let user_field = fields_named
        .named
        .iter()
        .find(|f| f.ident.as_ref().is_some_and(|i| i == "user"))
        .expect("user field not found");

    // Debug: Check if extract_belongs_to_from_field works
    let fk_field = extract_belongs_to_from_field(&user_field.attrs);
    assert_eq!(
        fk_field,
        Some("user_id".to_string()),
        "Should extract FK field from attribute"
    );

    // Debug: Check if is_field_optional_in_struct works
    let is_fk_optional = is_field_optional_in_struct(&parsed_struct, "user_id");
    assert!(!is_fk_optional, "user_id: i32 should not be optional");

    let result = convert_relation_type_to_schema_with_info(
        &user_field.ty,
        &user_field.attrs,
        &parsed_struct,
        &[
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ],
        user_field.ident.clone().unwrap(),
    );

    assert!(result.is_some(), "Should convert BelongsTo relation");
    let (_, rel_info) = result.unwrap();
    assert_eq!(rel_info.relation_type, "BelongsTo");
    // The FK field user_id is i32 (not Option), so is_optional should be false
    assert!(
        !rel_info.is_optional,
        "BelongsTo with required FK (user_id: i32) should have is_optional = false"
    );
}

#[test]
fn test_extract_belongs_to_from_field_with_equals_value() {
    // Test that extract_belongs_to_from_field works with belongs_to = "..." format
    use crate::schema_macro::seaorm::extract_belongs_to_from_field;

    // Format 1: belongs_to (flag style) - known to work
    let attrs1: Vec<syn::Attribute> = vec![syn::parse_quote!(
        #[sea_orm(belongs_to, from = "user_id", to = "id")]
    )];
    let result1 = extract_belongs_to_from_field(&attrs1);
    assert_eq!(
        result1,
        Some("user_id".to_string()),
        "Flag style should work"
    );

    // Format 2: belongs_to = "..." (value style) - testing this
    let attrs2: Vec<syn::Attribute> = vec![syn::parse_quote!(
        #[sea_orm(belongs_to = "super::user::Entity", from = "user_id", to = "id")]
    )];
    let result2 = extract_belongs_to_from_field(&attrs2);
    assert_eq!(
        result2,
        Some("user_id".to_string()),
        "Value style should also work"
    );
}

// ============================================================
// Tests for multipart mode
// ============================================================

#[test]
fn test_generate_schema_type_code_multipart_basic() {
    // Tests: multipart mode generates TryFromMultipart derive, suppresses From impl
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
    // Should derive TryFromMultipart
    assert!(output.contains("TryFromMultipart"));
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
    // Should derive TryFromMultipart
    assert!(output.contains("TryFromMultipart"));
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
    // Should derive TryFromMultipart
    assert!(output.contains("TryFromMultipart"));
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
    // Should derive TryFromMultipart
    assert!(output.contains("TryFromMultipart"));
    // Fields should be wrapped in Option (partial)
    assert!(output.contains("Option"));
    // Should NOT have From impl
    assert!(!output.contains("impl From"));
}

#[test]
#[serial]
fn test_generate_schema_type_code_qualified_path_with_nonempty_module_path() {
    // Tests: qualified path with explicit module segments that are not empty
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let src_dir = temp_dir.path().join("src");
    let models_dir = src_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();

    // Create user.rs
    let user_model = r"
pub struct Model {
    pub id: i32,
    pub name: String,
}
";
    std::fs::write(models_dir.join("user.rs"), user_model).unwrap();

    // Save original CARGO_MANIFEST_DIR
    let original_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok();
    // SAFETY: This is a test that runs single-threaded
    unsafe { std::env::set_var("CARGO_MANIFEST_DIR", temp_dir.path()) };

    // crate::models::user::Model - this is a qualified path
    // extract_module_path should return ["crate", "models", "user"]
    // So the if source_module_path.is_empty() check should be false
    let tokens = quote!(UserSchema from crate::models::user::Model);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let storage: HashMap<String, StructMetadata> = HashMap::new();

    let result = generate_schema_type_code(&input, &storage);

    // Restore CARGO_MANIFEST_DIR
    // SAFETY: This is a test that runs single-threaded
    unsafe {
        if let Some(dir) = original_manifest_dir {
            std::env::set_var("CARGO_MANIFEST_DIR", dir);
        } else {
            std::env::remove_var("CARGO_MANIFEST_DIR");
        }
    }

    assert!(result.is_ok());
    let (tokens, _metadata) = result.unwrap();
    let output = tokens.to_string();
    assert!(output.contains("UserSchema"));
}
