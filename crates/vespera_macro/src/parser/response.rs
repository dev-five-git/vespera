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
mod tests;
