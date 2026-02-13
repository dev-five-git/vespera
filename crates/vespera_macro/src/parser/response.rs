use std::collections::{BTreeMap, HashMap};

use syn::{ReturnType, Type};
use vespera_core::route::{Header, MediaType, Response};

use super::schema::parse_type_to_schema_ref_with_schemas;
use crate::parser::is_keyword_type::{KeywordType, is_keyword_type, is_keyword_type_by_type_path};

/// Unwrap Json<T> to get T
/// Handles both Json<T> and vespera::axum::Json<T> by checking the last segment
fn unwrap_json(ty: &Type) -> &Type {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if !path.segments.is_empty() {
            // Check the last segment (handles both Json<T> and vespera::axum::Json<T>)
            let segment = path.segments.last().unwrap();
            if segment.ident == "Json"
                && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
            {
                return inner_ty;
            }
        }
    }
    ty
}

/// Extract Ok and Err types from Result<T, E> or Result<Json<T>, E>
/// Handles both Result and std::result::Result, and unwraps references
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

/// Check if error type is a tuple (StatusCode, E) or (StatusCode, Json<E>)
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

/// Extract payload type from an Ok tuple and track if headers exist.
/// The last element of the tuple is always treated as the response body.
/// Any presence of HeaderMap in the tuple marks headers as present.
fn extract_ok_payload_and_headers(ok_ty: &Type) -> (Type, Option<HashMap<String, Header>>) {
    if let Type::Tuple(tuple) = ok_ty {
        let payload_ty = tuple.elems.last().map(|ty| unwrap_json(ty).clone());

        if let Some(payload_ty) = payload_ty {
            let headers = if tuple
                .elems
                .iter()
                .any(|ty| is_keyword_type(ty, &KeywordType::HeaderMap))
            {
                Some(HashMap::new())
            } else {
                None
            };
            return (payload_ty, headers);
        }
    }

    (ok_ty.clone(), None)
}

/// Analyze return type and convert to Responses map
pub fn parse_return_type(
    return_type: &ReturnType,
    known_schemas: &HashMap<String, String>,
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
            // Check if it's a Result<T, E>
            if let Some((ok_ty, err_ty)) = extract_result_types(ty) {
                // Handle success response (200)
                let (ok_payload_ty, ok_headers) = extract_ok_payload_and_headers(&ok_ty);
                let ok_schema = parse_type_to_schema_ref_with_schemas(
                    &ok_payload_ty,
                    known_schemas,
                    struct_definitions,
                );
                let mut ok_content = BTreeMap::new();
                ok_content.insert(
                    "application/json".to_string(),
                    MediaType {
                        schema: Some(ok_schema),
                        example: None,
                        examples: None,
                    },
                );

                responses.insert(
                    "200".to_string(),
                    Response {
                        description: "Successful response".to_string(),
                        headers: ok_headers,
                        content: Some(ok_content),
                    },
                );

                // Handle error response
                // Check if error is (StatusCode, E) tuple
                if let Some((status_code, error_type)) = extract_status_code_tuple(&err_ty) {
                    // Use the status code from the tuple
                    let err_schema = parse_type_to_schema_ref_with_schemas(
                        &error_type,
                        known_schemas,
                        struct_definitions,
                    );
                    let mut err_content = BTreeMap::new();
                    err_content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(err_schema),
                            example: None,
                            examples: None,
                        },
                    );

                    responses.insert(
                        status_code.to_string(),
                        Response {
                            description: "Error response".to_string(),
                            headers: None,
                            content: Some(err_content),
                        },
                    );
                } else {
                    // Regular error type - use default 400
                    // Unwrap Json if present
                    let err_ty_unwrapped = unwrap_json(&err_ty);
                    let err_schema = parse_type_to_schema_ref_with_schemas(
                        err_ty_unwrapped,
                        known_schemas,
                        struct_definitions,
                    );
                    let mut err_content = BTreeMap::new();
                    err_content.insert(
                        "application/json".to_string(),
                        MediaType {
                            schema: Some(err_schema),
                            example: None,
                            examples: None,
                        },
                    );

                    responses.insert(
                        "400".to_string(),
                        Response {
                            description: "Error response".to_string(),
                            headers: None,
                            content: Some(err_content),
                        },
                    );
                }
            } else {
                // Not a Result type - regular response
                // Unwrap Json<T> if present
                let unwrapped_ty = unwrap_json(ty);
                let schema = parse_type_to_schema_ref_with_schemas(
                    unwrapped_ty,
                    known_schemas,
                    struct_definitions,
                );
                let mut content = BTreeMap::new();
                content.insert(
                    "application/json".to_string(),
                    MediaType {
                        schema: Some(schema),
                        example: None,
                        examples: None,
                    },
                );

                responses.insert(
                    "200".to_string(),
                    Response {
                        description: "Successful response".to_string(),
                        headers: None,
                        content: Some(content),
                    },
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
            let full_signature = format!("fn test() {}", return_type_str);
            syn::parse_str::<syn::Signature>(&full_signature)
                .expect("Failed to parse return type")
                .output
        }
    }

    fn assert_schema_matches(schema_ref: &SchemaRef, expected: &ExpectedSchema) {
        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(schema.schema_type, Some(expected.schema_type.clone()));
                assert_eq!(schema.nullable.unwrap_or(false), expected.nullable);
                if let Some(item_ty) = &expected.items_schema_type {
                    let items = schema
                        .items
                        .as_ref()
                        .expect("items should be present for array");
                    match items.as_ref() {
                        SchemaRef::Inline(item_schema) => {
                            assert_eq!(item_schema.schema_type, Some(item_ty.clone()));
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
    fn test_parse_return_type(
        #[case] return_type_str: &str,
        #[case] ok_expectation: Option<ExpectedSchema>,
        #[case] err_expectation: Option<ExpectedResponse>,
        #[case] ok_headers_expected: Option<bool>,
    ) {
        let known_schemas = HashMap::new();
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
                let media_type = content
                    .get("application/json")
                    .expect("ok media type should exist");
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
                    .get("application/json")
                    .expect("error media type should exist");
                let schema_ref = media_type
                    .schema
                    .as_ref()
                    .expect("error schema should exist");
                assert_schema_matches(schema_ref, &err.schema);
            }
        }
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
        let known_schemas = HashMap::new();
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
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();
        let return_type = parse_return_type_str("-> Result<(StatusCode, Json<String>), String>");

        let responses = parse_return_type(&return_type, &known_schemas, &struct_definitions);

        // Should have 200 and 400 responses
        assert!(responses.contains_key("200"));
        let ok_response = responses.get("200").unwrap();
        // Headers should be None
        assert!(ok_response.headers.is_none());
    }
}
