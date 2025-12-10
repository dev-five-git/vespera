use std::collections::{BTreeMap, HashMap};

use syn::{ReturnType, Type};
use vespera_core::route::{Header, MediaType, Response};

use super::schema::parse_type_to_schema_ref_with_schemas;

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
    let result_type = match unwrapped {
        Type::Path(type_path) => type_path,
        Type::Reference(type_ref) => {
            // Unwrap reference and check if it's a Result
            if let Type::Path(type_path) = type_ref.elem.as_ref() {
                type_path
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let path = &result_type.path;
    if path.segments.is_empty() {
        return None;
    }

    // Check if any segment is "Result" (handles both Result and std::result::Result)
    let is_result = path.segments.iter().any(|seg| seg.ident == "Result");

    if is_result {
        // Get the last segment (Result) to check for generics
        if let Some(segment) = path.segments.last()
            && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && args.args.len() >= 2
            && let (
                Some(syn::GenericArgument::Type(ok_ty)),
                Some(syn::GenericArgument::Type(err_ty)),
            ) = (args.args.first(), args.args.get(1))
        {
            // Unwrap Json from Ok type if present
            let ok_ty_unwrapped = unwrap_json(ok_ty);
            return Some((ok_ty_unwrapped.clone(), err_ty.clone()));
        }
    }
    None
}

/// Check if error type is a tuple (StatusCode, E) or (StatusCode, Json<E>)
/// Returns the error type E and a default status code (400)
fn extract_status_code_tuple(err_ty: &Type) -> Option<(u16, Type)> {
    if let Type::Tuple(tuple) = err_ty
        && tuple.elems.len() == 2
        && let Type::Path(type_path) = &tuple.elems[0]&& !&type_path.path.segments.is_empty() {
            let path = &type_path.path;
            let segment = &path.segments[0];
            // Check if it's StatusCode (could be qualified like axum::http::StatusCode)
            let is_status_code = segment.ident == "StatusCode"
                || (path.segments.len() > 1
                    && path.segments.iter().any(|s| s.ident == "StatusCode"));

            if is_status_code {
                // Use 400 as default status code
                // The actual status code value is determined at runtime
                if let Some(error_type) = tuple.elems.get(1) {
                    // Unwrap Json if present
                    let error_type_unwrapped = unwrap_json(error_type);
                    return Some((400, error_type_unwrapped.clone()));
                }
            }
    }
    None
}

fn is_header_map_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return false;
        }
        return path.segments.iter().any(|s| s.ident == "HeaderMap");
    }
    false
}

/// Extract payload type from an Ok tuple and track if headers exist.
/// The last element of the tuple is always treated as the response body.
/// Any presence of HeaderMap in the tuple marks headers as present.
fn extract_ok_payload_and_headers(ok_ty: &Type) -> (Type, Option<HashMap<String, Header>>) {
    if let Type::Tuple(tuple) = ok_ty {
        let payload_ty = tuple.elems.last().map(|ty| unwrap_json(ty).clone());
        let has_headers = tuple.elems.iter().any(is_header_map_type);

        if let Some(payload_ty) = payload_ty {
            let headers = if has_headers {
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
    use super::*;
    use rstest::rstest;
    use std::collections::HashMap;
    use vespera_core::schema::{SchemaRef, SchemaType};

    #[rstest]
    #[case("", "")] // No return type
    #[case("-> String", "String")] // Simple return type
    #[case("-> i32", "i32")] // Integer return type
    #[case("-> bool", "bool")] // Boolean return type
    #[case("-> Vec<String>", "Vec<String>")] // Array return type
    #[case("-> Option<String>", "Option<String>")] // Option return type
    #[case("-> Result<String, String>", "Result<String, String>")] // Result with same types
    #[case("-> Result<i32, String>", "Result<i32, String>")] // Result with different types
    #[case("-> Result<Json<User>, String>", "Result<Json<User>, String>")] // Result with Json wrapper
    #[case(
        "-> Result<String, (StatusCode, String)>",
        "Result<String, (StatusCode, String)>"
    )] // Result with status code tuple
    #[case("-> &str", "&str")] // Reference return type
    #[case("-> Result<&str, String>", "Result<&str, String>")] // Result with reference
    fn test_parse_return_type(#[case] return_type_str: &str, #[case] expected_type: &str) {
        let known_schemas = HashMap::new();
        let struct_definitions = HashMap::new();

        let return_type = if return_type_str.is_empty() {
            syn::ReturnType::Default
        } else {
            // Parse the return type from string
            let full_signature = format!("fn test() {}", return_type_str);
            let parsed: syn::Signature =
                syn::parse_str(&full_signature).expect("Failed to parse return type");
            parsed.output
        };

        let responses = parse_return_type(&return_type, &known_schemas, &struct_definitions);

        match expected_type {
            "" => {
                // ReturnType::Default - should have 200 with no content
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_none());
            }
            "String" | "&str" => {
                // String return type - should have 200 with String schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema");
                }
            }
            "i32" => {
                // Integer return type - should have 200 with Integer schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Integer));
                } else {
                    panic!("Expected inline Integer schema");
                }
            }
            "bool" => {
                // Boolean return type - should have 200 with Boolean schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Boolean));
                } else {
                    panic!("Expected inline Boolean schema");
                }
            }
            "Vec<String>" => {
                // Array return type - should have 200 with Array schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Array));
                    assert!(schema.items.is_some());
                    // Check that items is String
                    if let Some(items) = &schema.items {
                        if let SchemaRef::Inline(items_schema) = items.as_ref() {
                            assert_eq!(items_schema.schema_type, Some(SchemaType::String));
                        }
                    }
                } else {
                    panic!("Expected inline Array schema");
                }
            }
            "Option<String>" => {
                // Option return type - should have 200 with nullable String schema
                assert_eq!(responses.len(), 1);
                assert!(responses.contains_key("200"));
                let response = responses.get("200").unwrap();
                assert_eq!(response.description, "Successful response");
                assert!(response.content.is_some());
                let content = response.content.as_ref().unwrap();
                assert!(content.contains_key("application/json"));
                let media_type = content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.nullable, Some(true));
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline nullable String schema");
                }
            }
            "Result<String, String>" => {
                // Result types - should have 200 for Ok and 400 for Err
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Ok type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<i32, String>" => {
                // Result types - should have 200 for Ok and 400 for Err
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Integer));
                } else {
                    panic!("Expected inline Integer schema for Ok type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<Json<User>, String>" => {
                // Result with Json wrapper - should unwrap Json
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                // User is not in known_schemas, so it should be an object schema
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::Object));
                } else {
                    panic!("Expected inline Object schema for User type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<&str, String>" => {
                // Result with reference - should handle reference correctly
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400"));

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                assert!(ok_response.content.is_some());
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for &str type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                assert!(err_response.content.is_some());
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            "Result<String, (StatusCode, String)>" => {
                // Result with status code tuple - should use status code from tuple
                assert_eq!(responses.len(), 2);
                assert!(responses.contains_key("200"));
                assert!(responses.contains_key("400")); // Default status code from tuple

                let ok_response = responses.get("200").unwrap();
                assert_eq!(ok_response.description, "Successful response");
                let ok_content = ok_response.content.as_ref().unwrap();
                let ok_media_type = ok_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = ok_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Ok type");
                }

                let err_response = responses.get("400").unwrap();
                assert_eq!(err_response.description, "Error response");
                let err_content = err_response.content.as_ref().unwrap();
                let err_media_type = err_content.get("application/json").unwrap();
                if let SchemaRef::Inline(schema) = err_media_type.schema.as_ref().unwrap() {
                    assert_eq!(schema.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected inline String schema for Err type");
                }
            }
            _ => panic!("Unexpected test case"),
        }
    }
}

