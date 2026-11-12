use std::collections::{BTreeMap, HashMap, HashSet};

use syn::{ReturnType, Type};
use vespera_core::route::{Header, MediaType, Response};
use vespera_core::schema::{Schema, SchemaRef};

use super::schema::parse_type_to_schema_ref;
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
/// `[(HeaderName, &str); N]` header arrays are axum `IntoResponseParts` too.
fn is_non_body_type(ty: &Type) -> bool {
    matches!(ty, Type::Array(array) if matches!(array.elem.as_ref(), Type::Tuple(_)))
        || is_keyword_type(ty, &KeywordType::StatusCode)
        || is_keyword_type(ty, &KeywordType::HeaderMap)
        || is_keyword_type(ty, &KeywordType::CookieJar)
}

/// Extract payload type from an Ok tuple and track if headers exist.
/// Non-body types (`StatusCode`, `HeaderMap`, `CookieJar`) are filtered out.
/// The last remaining element is treated as the response body (`Json<T>` is
/// unwrapped). A tuple made only of metadata (`(StatusCode, CookieJar)`) has
/// no body, so the payload is `None`. Any presence of `HeaderMap` in the
/// tuple marks headers as present. A non-tuple type is its own payload.
fn extract_ok_payload_and_headers(
    ok_ty: &Type,
) -> (Option<Type>, Option<BTreeMap<String, Header>>) {
    if let Type::Tuple(tuple) = ok_ty
        && !tuple.elems.is_empty()
    {
        // Find the body type: last element that is NOT a non-body type
        let payload_ty = tuple
            .elems
            .iter()
            .rev()
            .find(|ty| !is_non_body_type(ty))
            .map(|ty| unwrap_json(ty).clone());
        let headers = tuple
            .elems
            .iter()
            .any(|ty| is_keyword_type(ty, &KeywordType::HeaderMap))
            .then(BTreeMap::new);
        return (payload_ty, headers);
    }

    (Some(unwrap_json(ok_ty).clone()), None)
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

/// Media type of a raw-bytes body.
const OCTET_STREAM: &str = "application/octet-stream";
/// Media type of an `Html<T>` body.
const TEXT_HTML: &str = "text/html";
/// Media type of an `Sse<S>` body.
const EVENT_STREAM: &str = "text/event-stream";

/// True for raw byte bodies axum sends as `application/octet-stream`:
/// `Bytes`, `BytesMut`, `Body`, `Vec<u8>`, `Box<[u8]>` and `&[u8]`.
fn is_bytes_like(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => {
            matches!(reference.elem.as_ref(), Type::Slice(slice) if is_u8(&slice.elem))
        }
        Type::Path(type_path) => type_path.path.segments.last().is_some_and(|seg| {
            match seg.ident.to_string().as_str() {
                "Bytes" | "BytesMut" | "Body" => true,
                "Vec" | "Box" => matches!(&seg.arguments, syn::PathArguments::AngleBracketed(args)
                    if matches!(args.args.first(), Some(syn::GenericArgument::Type(inner))
                        if is_u8(inner) || matches!(inner, Type::Slice(slice) if is_u8(&slice.elem)))),
                _ => false,
            }
        }),
        _ => false,
    }
}

fn is_u8(ty: &Type) -> bool {
    matches!(ty, Type::Path(type_path) if type_path.path.is_ident("u8"))
}

/// True for bodies whose shape the signature does not reveal: `impl
/// IntoResponse` (any `impl Trait`), a type-erased `Response` and a
/// `Redirect` (no body at all). They are documented by description only.
fn is_opaque_body(ty: &Type) -> bool {
    match ty {
        Type::ImplTrait(_) => true,
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "Response" || seg.ident == "Redirect"),
        _ => false,
    }
}

/// The last path segment's ident of `ty`, if it is a path.
fn last_ident_is(ty: &Type, ident: &str) -> bool {
    matches!(ty, Type::Path(type_path)
        if type_path.path.segments.last().is_some_and(|seg| seg.ident == ident))
}

/// The response `Content-Type` for a body of the given original (pre-`unwrap_json`)
/// type, or `None` when the body is opaque (no `content` is documented):
/// bare strings are `text/plain`, `Html<T>` is `text/html`, `Sse<S>` is
/// `text/event-stream`, raw bytes are `application/octet-stream`; `Json<T>`
/// and structs are `application/json`.
fn body_content_type(ty: &Type) -> Option<&'static str> {
    if is_opaque_body(ty) {
        None
    } else if is_string_like(ty) {
        Some("text/plain")
    } else if last_ident_is(ty, "Html") {
        Some(TEXT_HTML)
    } else if last_ident_is(ty, "Sse") {
        Some(EVENT_STREAM)
    } else if is_bytes_like(ty) {
        Some(OCTET_STREAM)
    } else {
        Some("application/json")
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

/// `(200-body content-type, error-body content-type)` for a handler return
/// type (see [`body_content_type`]; `None` = opaque, no content).
fn response_content_types(ty: &Type) -> (Option<&'static str>, Option<&'static str>) {
    if let Some((ok, err)) = result_args(ty) {
        (
            body_content_type(tuple_body(ok)),
            body_content_type(tuple_body(err)),
        )
    } else {
        (body_content_type(tuple_body(ty)), Some("application/json"))
    }
}

fn content_for_type(
    ty: &Type,
    content_type: Option<&str>,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<BTreeMap<String, MediaType>> {
    let content_type = content_type?;
    if is_keyword_type(ty, &KeywordType::StatusCode) {
        return None;
    }

    // Html / Sse / raw-byte bodies are strings on the wire regardless of the
    // Rust payload type; bytes are documented as `format: binary`.
    let schema = match content_type {
        TEXT_HTML | EVENT_STREAM => SchemaRef::Inline(Box::new(Schema::string())),
        OCTET_STREAM => SchemaRef::Inline(Box::new(Schema {
            format: Some("binary".to_string()),
            ..Schema::string()
        })),
        _ => parse_type_to_schema_ref(ty, known_schemas, struct_definitions),
    };
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
    ok_content_type: Option<&str>,
    err_content_type: Option<&str>,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) {
    let (ok_payload_ty, ok_headers) = extract_ok_payload_and_headers(ok_ty);
    let ok_content = ok_payload_ty.and_then(|payload| {
        content_for_type(&payload, ok_content_type, known_schemas, struct_definitions)
    });
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
    content_type: Option<&str>,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) {
    // Same tuple handling as the `Result` Ok side: `(StatusCode, Json<T>)` and
    // `(StatusCode, HeaderMap, Json<T>)` document `T`, never the tuple itself.
    let (payload_ty, headers) = extract_ok_payload_and_headers(ty);
    let content = payload_ty.and_then(|payload| {
        content_for_type(&payload, content_type, known_schemas, struct_definitions)
    });
    responses.insert("200".to_string(), successful_response(content, headers));
}

/// Analyze return type and convert to Responses map
pub fn parse_return_type(
    return_type: &ReturnType,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
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
