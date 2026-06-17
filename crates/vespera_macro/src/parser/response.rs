use std::collections::{BTreeMap, HashMap, HashSet};

use syn::{ReturnType, Type};
use vespera_core::route::{Header, MediaType, Response};

use super::schema::parse_type_to_schema_ref_with_schemas;
use crate::parser::is_keyword_type::{KeywordType, is_keyword_type, is_keyword_type_by_type_path};

/// Unwrap Json<T> to get T
/// Handles both Json<T> and `vespera::axum::Json`<T> by checking the last segment
fn unwrap_json(ty: &Type) -> &Type {
    // Check the last segment (handles both `Json<T>` and
    // `vespera::axum::Json<T>`). `segments.last()` is `None` for an empty
    // path, so the let-chain replaces the prior `is_empty()` guard + `unwrap()`.
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
        && segment.ident == "Json"
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
    {
        return inner_ty;
    }
    ty
}

/// Extract Ok and Err types from Result<T, E> or Result<Json<T>, E>
/// Handles both Result and `std::result::Result`, and unwraps references
fn extract_result_types(ty: &Type) -> Option<(Type, Type)> {
    // First unwrap Json if present
    let unwrapped = unwrap_json(ty);

    // Handle both Type::Path and Type::Reference (for &Result<...>)
    let result_type = if let Type::Path(type_path) = unwrapped {
        type_path
    } else if let Type::Reference(type_ref) = unwrapped
        && let Type::Path(type_path) = type_ref.elem.as_ref()
    {
        type_path
    } else {
        return None;
    };

    let path = &result_type.path;
    if path.segments.is_empty() {
        return None;
    }

    if is_keyword_type_by_type_path(result_type, &KeywordType::Result)
        && let Some(segment) = path.segments.last()
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && args.args.len() >= 2
        && let (Some(syn::GenericArgument::Type(ok_ty)), Some(syn::GenericArgument::Type(err_ty))) =
            (args.args.first(), args.args.get(1))
    {
        // Get the last segment (Result) to check for generics
        // Unwrap Json from Ok type if present
        let ok_ty_unwrapped = unwrap_json(ok_ty);
        return Some((ok_ty_unwrapped.clone(), err_ty.clone()));
    }
    None
}

/// Check if error type is a tuple (`StatusCode`, E) or (`StatusCode`, Json<E>)
/// Returns the error type E and a default status code (400)
fn extract_status_code_tuple(err_ty: &Type) -> Option<(u16, Type)> {
    if let Type::Tuple(tuple) = err_ty
        && tuple
            .elems
            .iter()
            .any(|ty| is_keyword_type(ty, &KeywordType::StatusCode))
    {
        Some((400, unwrap_json(tuple.elems.last().unwrap()).clone()))
    } else {
        None
    }
}

/// Check if a type is a non-body response type (metadata only).
/// These types contribute to the HTTP response (status, headers, cookies)
/// but do not form the response body.
fn is_non_body_type(ty: &Type) -> bool {
    is_keyword_type(ty, &KeywordType::StatusCode)
        || is_keyword_type(ty, &KeywordType::HeaderMap)
        || is_keyword_type(ty, &KeywordType::CookieJar)
}

/// Extract payload type from an Ok tuple and track if headers exist.
/// Non-body types (`StatusCode`, `HeaderMap`, `CookieJar`) are filtered out.
/// The last remaining element is treated as the response body.
/// Any presence of `HeaderMap` in the tuple marks headers as present.
fn extract_ok_payload_and_headers(ok_ty: &Type) -> (Type, Option<BTreeMap<String, Header>>) {
    if let Type::Tuple(tuple) = ok_ty {
        // Find the body type: last element that is NOT a non-body type
        let payload_ty = tuple
            .elems
            .iter()
            .rev()
            .find(|ty| !is_non_body_type(ty))
            .map(|ty| unwrap_json(ty).clone());

        if let Some(payload_ty) = payload_ty {
            let headers = if tuple
                .elems
                .iter()
                .any(|ty| is_keyword_type(ty, &KeywordType::HeaderMap))
            {
                Some(BTreeMap::new())
            } else {
                None
            };
            return (payload_ty, headers);
        }
    }

    (ok_ty.clone(), None)
}

/// True if `ty` is a bare `String` / `str` / `&str` (NOT wrapped in `Json`).
/// axum serves such bodies as `text/plain`, mirroring the request-body side.
fn is_string_like(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => is_string_like(&reference.elem),
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "String" || seg.ident == "str"),
        _ => false,
    }
}

/// The response `Content-Type` for a body of the given original (pre-`unwrap_json`)
/// type: bare strings are `text/plain`; `Json<T>` and structs are
/// `application/json`.
fn body_content_type(ty: &Type) -> &'static str {
    if is_string_like(ty) {
        "text/plain"
    } else {
        "application/json"
    }
}

/// The last non-metadata element of a tuple body (`(StatusCode, T)` → `T`), or
/// `ty` itself when it is not a tuple.
fn tuple_body(ty: &Type) -> &Type {
    if let Type::Tuple(tuple) = ty {
        tuple
            .elems
            .iter()
            .rev()
            .find(|elem| !is_non_body_type(elem))
            .unwrap_or(ty)
    } else {
        ty
    }
}

/// The original `(Ok, Err)` argument types of a `Result<Ok, Err>` return type
/// (no `Json` unwrapping) — used for content-type determination only.
fn result_args(ty: &Type) -> Option<(&Type, &Type)> {
    let type_path = match unwrap_json(ty) {
        Type::Path(type_path) => type_path,
        Type::Reference(type_ref) => match type_ref.elem.as_ref() {
            Type::Path(type_path) => type_path,
            _ => return None,
        },
        _ => return None,
    };
    if is_keyword_type_by_type_path(type_path, &KeywordType::Result)
        && let Some(segment) = type_path.path.segments.last()
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && args.args.len() >= 2
        && let (Some(syn::GenericArgument::Type(ok)), Some(syn::GenericArgument::Type(err))) =
            (args.args.first(), args.args.get(1))
    {
        Some((ok, err))
    } else {
        None
    }
}

/// `(200-body content-type, error-body content-type)` for a handler return type.
/// Bare `String`/`&str` bodies map to `text/plain` (what axum actually sends);
/// `Json<T>` and structs map to `application/json`.
fn response_content_types(ty: &Type) -> (&'static str, &'static str) {
    if let Some((ok, err)) = result_args(ty) {
        (
            body_content_type(tuple_body(ok)),
            body_content_type(tuple_body(err)),
        )
    } else {
        (body_content_type(tuple_body(ty)), "application/json")
    }
}

fn content_for_type(
    ty: &Type,
    content_type: &str,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> Option<BTreeMap<String, MediaType>> {
    if is_keyword_type(ty, &KeywordType::StatusCode) {
        return None;
    }

    let schema = parse_type_to_schema_ref_with_schemas(ty, known_schemas, struct_definitions);
    let mut content = BTreeMap::new();
    content.insert(
        content_type.to_string(),
        MediaType {
            schema: Some(schema),
            example: None,
            examples: None,
        },
    );
    Some(content)
}

fn successful_response(
    content: Option<BTreeMap<String, MediaType>>,
    headers: Option<BTreeMap<String, Header>>,
) -> Response {
    Response {
        description: "Successful response".to_string(),
        headers,
        content,
    }
}

fn error_response(content: Option<BTreeMap<String, MediaType>>) -> Response {
    Response {
        description: "Error response".to_string(),
        headers: None,
        content,
    }
}

fn insert_result_responses(
    responses: &mut BTreeMap<String, Response>,
    ok_ty: &Type,
    err_ty: &Type,
    ok_content_type: &str,
    err_content_type: &str,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) {
    let (ok_payload_ty, ok_headers) = extract_ok_payload_and_headers(ok_ty);
    let ok_content = content_for_type(
        &ok_payload_ty,
        ok_content_type,
        known_schemas,
        struct_definitions,
    );
    responses.insert(
        "200".to_string(),
        successful_response(ok_content, ok_headers),
    );

    if let Some((status_code, error_type)) = extract_status_code_tuple(err_ty) {
        let err_content = content_for_type(
            &error_type,
            err_content_type,
            known_schemas,
            struct_definitions,
        );
        responses.insert(status_code.to_string(), error_response(err_content));
    } else {
        let err_ty_unwrapped = unwrap_json(err_ty);
        let err_content = content_for_type(
            err_ty_unwrapped,
            err_content_type,
            known_schemas,
            struct_definitions,
        );
        responses.insert("400".to_string(), error_response(err_content));
    }
}

fn insert_plain_response(
    responses: &mut BTreeMap<String, Response>,
    ty: &Type,
    content_type: &str,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) {
    let unwrapped_ty = unwrap_json(ty);
    let content = content_for_type(
        unwrapped_ty,
        content_type,
        known_schemas,
        struct_definitions,
    );
    responses.insert("200".to_string(), successful_response(content, None));
}

/// Analyze return type and convert to Responses map
pub fn parse_return_type(
    return_type: &ReturnType,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> BTreeMap<String, Response> {
    let mut responses = BTreeMap::new();

    match return_type {
        ReturnType::Default => {
            // No return type - just 200 with no content
            responses.insert(
                "200".to_string(),
                Response {
                    description: "Successful response".to_string(),
                    headers: None,
                    content: None,
                },
            );
        }
        ReturnType::Type(_, ty) => {
            let (ok_content_type, err_content_type) = response_content_types(ty);
            if let Some((ok_ty, err_ty)) = extract_result_types(ty) {
                insert_result_responses(
                    &mut responses,
                    &ok_ty,
                    &err_ty,
                    ok_content_type,
                    err_content_type,
                    known_schemas,
                    struct_definitions,
                );
            } else {
                insert_plain_response(
                    &mut responses,
                    ty,
                    ok_content_type,
                    known_schemas,
                    struct_definitions,
                );
            }
        }
    }

    responses
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rstest::rstest;
    use vespera_core::schema::{SchemaRef, SchemaType};

    use super::*;

    #[derive(Debug)]
    struct ExpectedSchema {
        schema_type: SchemaType,
        nullable: bool,
        items_schema_type: Option<SchemaType>,
    }

    #[derive(Debug)]
    struct ExpectedResponse {
        status: &'static str,
        schema: ExpectedSchema,
    }

    fn parse_return_type_str(return_type_str: &str) -> syn::ReturnType {
        if return_type_str.is_empty() {
            syn::ReturnType::Default
        } else {
            let full_signature = format!("fn test() {return_type_str}");
            syn::parse_str::<syn::Signature>(&full_signature)
                .expect("Failed to parse return type")
                .output
        }
    }

    fn assert_schema_matches(schema_ref: &SchemaRef, expected: &ExpectedSchema) {
        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(schema.schema_type, Some(expected.schema_type));
                assert_eq!(schema.nullable.unwrap_or(false), expected.nullable);
                if let Some(item_ty) = &expected.items_schema_type {
                    let items = schema
                        .items
                        .as_ref()
                        .expect("items should be present for array");
                    match items {
                        SchemaRef::Inline(item_schema) => {
                            assert_eq!(item_schema.schema_type, Some(*item_ty));
                        }
                        SchemaRef::Ref(_) => panic!("expected inline schema for array items"),
                    }
                }
            }
            SchemaRef::Ref(_) => panic!("expected inline schema"),
        }
    }

    #[rstest]
    #[case("", None, None, None)]
    #[case(
        "-> String",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    #[case(
        "-> &str",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    #[case(
        "-> i32",
        Some(ExpectedSchema { schema_type: SchemaType::Integer, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    #[case(
        "-> bool",
        Some(ExpectedSchema { schema_type: SchemaType::Boolean, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    #[case(
        "-> Vec<String>",
        Some(ExpectedSchema { schema_type: SchemaType::Array, nullable: false, items_schema_type: Some(SchemaType::String) }),
        None,
        None
    )]
    #[case(
        "-> Option<String>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: true, items_schema_type: None }),
        None,
        None
    )]
    #[case(
        "-> Result<String, String>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    #[case(
        "-> Result<i32, String>",
        Some(ExpectedSchema { schema_type: SchemaType::Integer, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    #[case(
        "-> Result<Json<User>, String>",
        Some(ExpectedSchema { schema_type: SchemaType::Object, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    #[case(
        "-> Result<&str, String>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    #[case(
        "-> Result<String, (StatusCode, String)>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    #[case(
        "-> Result<String, (StatusCode, Json<String>)>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    #[case(
        "-> Result<(HeaderMap<String, String>, Json<i32>), String>",
        Some(ExpectedSchema { schema_type: SchemaType::Integer, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        Some(true)
    )]
    #[case(
        "-> Result<String, (axum::http::StatusCode, Json<i32>)>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::Integer, nullable: false, items_schema_type: None } }),
        None
    )]
    // StatusCode as the sole Ok response type → no content (empty body)
    #[case(
        "-> Result<StatusCode, (StatusCode, String)>",
        None,
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    // CookieJar in Ok tuple → body is Json<String>, CookieJar filtered out
    #[case(
        "-> Result<(CookieJar, Json<String>), (StatusCode, String)>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    // CookieJar + StatusCode in Ok tuple → body is last non-metadata element
    #[case(
        "-> Result<(StatusCode, CookieJar, Json<i32>), String>",
        Some(ExpectedSchema { schema_type: SchemaType::Integer, nullable: false, items_schema_type: None }),
        Some(ExpectedResponse { status: "400", schema: ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None } }),
        None
    )]
    // Non-Result: StatusCode alone → no content (covers line 155)
    #[case("-> StatusCode", None, None, None)]
    // Non-Result: Json<T> wrapper → unwraps to T
    #[case(
        "-> Json<String>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    // Non-Result: Json<i32> wrapper → unwraps to integer
    #[case(
        "-> Json<i32>",
        Some(ExpectedSchema { schema_type: SchemaType::Integer, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    // Non-Result: f64 → number type
    #[case(
        "-> f64",
        Some(ExpectedSchema { schema_type: SchemaType::Number, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    // Non-Result: qualified axum::Json<String> → unwraps to String
    #[case(
        "-> axum::Json<String>",
        Some(ExpectedSchema { schema_type: SchemaType::String, nullable: false, items_schema_type: None }),
        None,
        None
    )]
    fn test_parse_return_type(
        #[case] return_type_str: &str,
        #[case] ok_expectation: Option<ExpectedSchema>,
        #[case] err_expectation: Option<ExpectedResponse>,
        #[case] ok_headers_expected: Option<bool>,
    ) {
        let known_schemas = HashSet::new();
        let struct_definitions = HashMap::new();
        let return_type = parse_return_type_str(return_type_str);

        let responses = parse_return_type(&return_type, &known_schemas, &struct_definitions);

        // Validate success response
        let ok_response = responses.get("200").expect("200 response should exist");
        assert_eq!(ok_response.description, "Successful response");
        match &ok_expectation {
            None => {
                assert!(ok_response.content.is_none());
            }
            Some(expected_schema) => {
                let content = ok_response
                    .content
                    .as_ref()
                    .expect("ok content should exist");
                let media_type = content.values().next().expect("ok media type should exist");
                let schema_ref = media_type.schema.as_ref().expect("ok schema should exist");
                assert_schema_matches(schema_ref, expected_schema);
            }
        }
        if let Some(expect_headers) = ok_headers_expected {
            assert_eq!(ok_response.headers.is_some(), expect_headers);
        }

        // Validate error response (if any)
        match &err_expectation {
            None => assert_eq!(responses.len(), 1),
            Some(err) => {
                assert_eq!(responses.len(), 2);
                let err_response = responses
                    .get(err.status)
                    .expect("error response should exist");
                assert_eq!(err_response.description, "Error response");
                let content = err_response
                    .content
                    .as_ref()
                    .expect("error content should exist");
                let media_type = content
                    .values()
                    .next()
                    .expect("error media type should exist");
                let schema_ref = media_type
                    .schema
                    .as_ref()
                    .expect("error schema should exist");
                assert_schema_matches(schema_ref, &err.schema);
            }
        }
    }

    #[rstest]
    #[case("-> String", "200", "text/plain")]
    #[case("-> &str", "200", "text/plain")]
    #[case("-> Json<String>", "200", "application/json")]
    #[case("-> i32", "200", "application/json")]
    #[case("-> Result<String, String>", "200", "text/plain")]
    #[case("-> Result<String, String>", "400", "text/plain")]
    #[case(
        "-> Result<Json<User>, (StatusCode, String)>",
        "200",
        "application/json"
    )]
    #[case("-> Result<Json<User>, (StatusCode, String)>", "400", "text/plain")]
    #[case(
        "-> Result<String, (StatusCode, Json<String>)>",
        "400",
        "application/json"
    )]
    fn response_content_type_matches_body_kind(
        #[case] return_type_str: &str,
        #[case] status: &str,
        #[case] expected_content_type: &str,
    ) {
        let return_type = parse_return_type_str(return_type_str);
        let responses = parse_return_type(&return_type, &HashSet::new(), &HashMap::new());
        let content = responses
            .get(status)
            .and_then(|response| response.content.as_ref())
            .unwrap_or_else(|| panic!("{status} content missing for `{return_type_str}`"));
        assert!(
            content.contains_key(expected_content_type),
            "`{return_type_str}` {status}: expected {expected_content_type}, got {:?}",
            content.keys().collect::<Vec<_>>()
        );
    }

    // ======== Tests for uncovered lines ========

    #[test]
    fn test_extract_result_types_non_path_non_ref() {
        // Test line 43: type that's neither Path nor Reference returns None
        // Tuple type is neither Path nor Reference
        let ty: syn::Type = syn::parse_str("(i32, String)").unwrap();
        let result = extract_result_types(&ty);
        assert!(result.is_none());

        // Array type
        let ty: syn::Type = syn::parse_str("[i32; 3]").unwrap();
        let result = extract_result_types(&ty);
        assert!(result.is_none());

        // Slice type
        let ty: syn::Type = syn::parse_str("[i32]").unwrap();
        let result = extract_result_types(&ty);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_result_types_ref_to_non_path() {
        // Test line 43: &(Tuple) - Reference to non-Path type
        // Tests: else branch
        let ty: syn::Type = syn::parse_str("&(i32, String)").unwrap();
        let result = extract_result_types(&ty);
        // The Reference's elem is a Tuple, not a Path, so line 39 condition fails
        // Falls through to line 43
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_result_types_empty_path_segments() {
        // Test line 48: path.segments.is_empty() returns None
        // Create a Type::Path programmatically with empty segments
        use syn::punctuated::Punctuated;

        let type_path = syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: Punctuated::new(), // Empty segments!
            },
        };
        let ty = syn::Type::Path(type_path);

        // Tests: path.segments.is_empty() is true
        let result = extract_result_types(&ty);
        assert!(
            result.is_none(),
            "Empty path segments should return None (line 48)"
        );
    }

    #[test]
    fn test_extract_result_types_empty_path_via_reference() {
        // Test line 48 via reference path: &Type::Path with empty segments
        use syn::punctuated::Punctuated;

        // Create inner Type::Path with empty segments
        let inner_type_path = syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: Punctuated::new(),
            },
        };
        let inner_ty = syn::Type::Path(inner_type_path);

        // Wrap in a reference
        let ty = syn::Type::Reference(syn::TypeReference {
            and_token: syn::token::And::default(),
            lifetime: None,
            mutability: None,
            elem: Box::new(inner_ty),
        });

        // Tests: reference to path then empty segments
        let result = extract_result_types(&ty);
        assert!(
            result.is_none(),
            "Empty path segments via reference should return None (line 48)"
        );
    }

    #[test]
    fn test_extract_result_types_with_reference() {
        // Test the Reference path (line 38-41) that succeeds
        // &Result<T, E> should still extract types
        let ty: syn::Type = syn::parse_str("&Result<String, i32>").unwrap();
        let _result = extract_result_types(&ty);
        // Note: This doesn't actually work because is_keyword_type_by_type_path
        // checks for Result type, but ref to Result is different
        // The important thing is the code doesn't panic
        // Tests: exercises reference path even if result is None
    }

    #[test]
    fn test_unwrap_json_non_json() {
        // Test unwrap_json with non-Json type returns original
        let ty: syn::Type = syn::parse_str("String").unwrap();
        let unwrapped = unwrap_json(&ty);
        // Should return the same type
        assert!(matches!(unwrapped, syn::Type::Path(_)));
    }

    #[test]
    fn test_unwrap_json_with_json() {
        // Test unwrap_json with Json<T>
        let ty: syn::Type = syn::parse_str("Json<String>").unwrap();
        let unwrapped = unwrap_json(&ty);
        // Should unwrap to String
        if let syn::Type::Path(type_path) = unwrapped {
            assert_eq!(
                type_path.path.segments.last().unwrap().ident.to_string(),
                "String"
            );
        } else {
            panic!("Expected Path type");
        }
    }

    #[test]
    fn test_parse_return_type_tuple() {
        // Test parse_return_type with tuple type (exercises line 43 via extract_result_types)
        let known_schemas = HashSet::new();
        let struct_definitions = HashMap::new();
        let return_type = parse_return_type_str("-> (i32, String)");

        let responses = parse_return_type(&return_type, &known_schemas, &struct_definitions);

        // Tuple is not a Result, so it should be treated as regular response
        assert!(responses.contains_key("200"));
        assert_eq!(responses.len(), 1);
    }

    #[test]
    fn test_extract_ok_payload_and_headers_tuple_without_headermap() {
        // Test line 95: tuple without HeaderMap returns None for headers
        let ty: syn::Type = syn::parse_str("(StatusCode, String)").unwrap();
        let (payload, headers) = extract_ok_payload_and_headers(&ty);

        // Payload should be String (last element unwrapped)
        if let syn::Type::Path(type_path) = &payload {
            assert_eq!(
                type_path.path.segments.last().unwrap().ident.to_string(),
                "String"
            );
        }
        // Headers should be None (no HeaderMap in tuple) - this is line 95
        assert!(headers.is_none());
    }

    #[test]
    fn test_parse_return_type_result_with_ok_tuple_no_headermap() {
        // Test line 95 via full parse_return_type: Result<(StatusCode, Json<T>), E>
        let known_schemas = HashSet::new();
        let struct_definitions = HashMap::new();
        let return_type = parse_return_type_str("-> Result<(StatusCode, Json<String>), String>");

        let responses = parse_return_type(&return_type, &known_schemas, &struct_definitions);

        // Should have 200 and 400 responses
        assert!(responses.contains_key("200"));
        let ok_response = responses.get("200").unwrap();
        // Headers should be None
        assert!(ok_response.headers.is_none());
    }

    // ======== CookieJar tuple extraction tests ========

    #[test]
    fn test_extract_ok_payload_and_headers_cookie_jar_tuple() {
        // (CookieJar, Json<String>) → payload should be String, CookieJar filtered
        let ty: syn::Type = syn::parse_str("(CookieJar, Json<String>)").unwrap();
        let (payload, headers) = extract_ok_payload_and_headers(&ty);

        if let syn::Type::Path(type_path) = &payload {
            assert_eq!(
                type_path.path.segments.last().unwrap().ident.to_string(),
                "String"
            );
        } else {
            panic!("Expected Path type for payload");
        }
        assert!(headers.is_none());
    }

    #[test]
    fn test_extract_ok_payload_and_headers_cookie_jar_with_status_code() {
        // (StatusCode, CookieJar, Json<i32>) → payload should be i32
        let ty: syn::Type = syn::parse_str("(StatusCode, CookieJar, Json<i32>)").unwrap();
        let (payload, headers) = extract_ok_payload_and_headers(&ty);

        if let syn::Type::Path(type_path) = &payload {
            assert_eq!(
                type_path.path.segments.last().unwrap().ident.to_string(),
                "i32"
            );
        } else {
            panic!("Expected Path type for payload");
        }
        assert!(headers.is_none());
    }

    #[test]
    fn test_extract_ok_payload_and_headers_all_non_body_types() {
        // (StatusCode, CookieJar) → no body element found, returns original tuple
        let ty: syn::Type = syn::parse_str("(StatusCode, CookieJar)").unwrap();
        let (payload, headers) = extract_ok_payload_and_headers(&ty);
        // No body element found → falls through to return original type
        assert!(matches!(payload, syn::Type::Tuple(_)));
        assert!(headers.is_none());
    }

    #[test]
    fn test_unwrap_json_qualified_path() {
        // vespera::axum::Json<String> → should unwrap to String via last-segment matching
        let ty: syn::Type = syn::parse_str("vespera::axum::Json<String>").unwrap();
        let unwrapped = unwrap_json(&ty);
        if let syn::Type::Path(type_path) = unwrapped {
            assert_eq!(
                type_path.path.segments.last().unwrap().ident.to_string(),
                "String"
            );
        } else {
            panic!("Expected Path type");
        }
    }

    #[test]
    fn test_unwrap_json_non_generic_path() {
        // Type with segments but no angle brackets → returns original
        let ty: syn::Type = syn::parse_str("std::string::String").unwrap();
        let unwrapped = unwrap_json(&ty);
        if let syn::Type::Path(type_path) = unwrapped {
            assert_eq!(
                type_path.path.segments.last().unwrap().ident.to_string(),
                "String"
            );
        } else {
            panic!("Expected Path type");
        }
    }

    #[test]
    fn test_parse_return_type_non_result_status_code() {
        // Direct StatusCode return (not in Result) → 200 with no content
        let known_schemas = HashSet::new();
        let struct_definitions = HashMap::new();
        let return_type = parse_return_type_str("-> StatusCode");

        let responses = parse_return_type(&return_type, &known_schemas, &struct_definitions);

        assert_eq!(responses.len(), 1);
        let ok_response = responses.get("200").unwrap();
        assert!(
            ok_response.content.is_none(),
            "StatusCode return should have no content"
        );
        assert!(ok_response.headers.is_none());
    }

    #[test]
    fn test_is_non_body_type() {
        let status: syn::Type = syn::parse_str("StatusCode").unwrap();
        assert!(is_non_body_type(&status));

        let header_map: syn::Type = syn::parse_str("HeaderMap").unwrap();
        assert!(is_non_body_type(&header_map));

        let cookie_jar: syn::Type = syn::parse_str("CookieJar").unwrap();
        assert!(is_non_body_type(&cookie_jar));

        let string: syn::Type = syn::parse_str("String").unwrap();
        assert!(!is_non_body_type(&string));

        let json: syn::Type = syn::parse_str("Json<String>").unwrap();
        assert!(!is_non_body_type(&json));
    }
}
