use std::collections::{BTreeMap, HashMap, HashSet};

use syn::{ReturnType, Type};
use vespera_core::route::{MediaType, Response};
use vespera_core::schema::{Schema, SchemaRef};

use super::schema::{is_builtin_openapi_type, parse_type_to_schema_ref};
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

/// Extract the Ok and Err types from `Result<T, E>` (`std::result::Result`
/// too, through a reference). `T` is kept as written: a `Json<(A, B)>` body
/// is a JSON array, not a `(A, B)` response tuple.
///
/// A one-argument `Result<T>` is axum's `axum::response::Result<T>` (error
/// type `axum::response::ErrorResponse`): its error body is opaque. User
/// aliases such as `type ApiResult<T> = Result<Json<T>, AppError>` are
/// expanded before this point (see [`super::result_alias`]).
fn extract_result_types(ty: &Type) -> Option<(Type, Type)> {
    let (ok_ty, err_ty) = result_args(ty)?;
    let err_ty = err_ty.cloned().unwrap_or_else(axum_error_response);
    Some((ok_ty.clone(), err_ty))
}

/// `axum::response::ErrorResponse` — the error half of a one-argument
/// `axum::response::Result<T>`.
fn axum_error_response() -> Type {
    syn::parse_quote!(axum::response::ErrorResponse)
}

/// Idents of axum / axum-extra `IntoResponseParts` types: they set headers,
/// cookies or extensions but never the response body.
const RESPONSE_PART_IDENTS: [&str; 6] = [
    "AppendHeaders",
    "TypedHeader",
    "Extension",
    "Extensions",
    "PrivateCookieJar",
    "SignedCookieJar",
];

/// Check if a type is a non-body response type (metadata only).
/// These types contribute to the HTTP response (status, headers, cookies,
/// extensions) but do not form the response body.
/// `[(HeaderName, &str); N]` header arrays are axum `IntoResponseParts` too.
/// A bare `()` is axum's empty response (no body at all), unlike `Json<()>`,
/// which serializes `null`.
fn is_non_body_type(ty: &Type) -> bool {
    is_unit(ty)
        || matches!(ty, Type::Array(array) if matches!(array.elem.as_ref(), Type::Tuple(_)))
        || is_keyword_type(ty, &KeywordType::StatusCode)
        || is_keyword_type(ty, &KeywordType::HeaderMap)
        || is_keyword_type(ty, &KeywordType::CookieJar)
        || RESPONSE_PART_IDENTS
            .iter()
            .any(|ident| last_ident_is(ty, ident))
}

/// The element of a response type that forms the body, as written (`Json`
/// not unwrapped): the last non-metadata element of a tuple
/// (`(StatusCode, HeaderMap, Json<T>)` → `Json<T>`), `None` when the type is
/// metadata only (`()`, `StatusCode`, `HeaderMap`, `(StatusCode, CookieJar)`),
/// otherwise the type itself.
fn body_element(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Tuple(tuple) => tuple
            .elems
            .iter()
            .rev()
            .find(|elem| !is_non_body_type(elem)),
        _ if is_non_body_type(ty) => None,
        _ => Some(ty),
    }
}

/// Whether a response type sets headers through a `HeaderMap` (bare or as a
/// tuple element).
fn has_header_map(ty: &Type) -> bool {
    match ty {
        Type::Tuple(tuple) => tuple
            .elems
            .iter()
            .any(|elem| is_keyword_type(elem, &KeywordType::HeaderMap)),
        _ => is_keyword_type(ty, &KeywordType::HeaderMap),
    }
}

/// `()` — the empty tuple.
fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(tuple) if tuple.elems.is_empty())
}

/// The first type argument of `ty`'s last path segment when that segment is
/// `ident` (`Box<str>` → `str`, `Cow<'static, str>` → `str`).
fn type_arg_of<'a>(ty: &'a Type, ident: &str) -> Option<&'a Type> {
    let segment = crate::schema_macro::type_utils::last_path_segment(ty)?;
    if segment.ident != ident {
        return None;
    }
    crate::schema_macro::type_utils::first_generic_type_arg(segment)
}

/// True if `ty` is a bare `String` / `str` / `&str` / `Box<str>` /
/// `Cow<'static, str>` (NOT wrapped in `Json`). axum serves such bodies as
/// `text/plain`, mirroring the request-body side.
fn is_string_like(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => is_string_like(&reference.elem),
        Type::Path(type_path) => {
            type_path
                .path
                .segments
                .last()
                .is_some_and(|seg| seg.ident == "String" || seg.ident == "str")
                || ["Box", "Cow"].iter().any(|wrapper| {
                    type_arg_of(ty, wrapper).is_some_and(
                        |inner| matches!(inner, Type::Path(p) if p.path.is_ident("str")),
                    )
                })
        }
        _ => false,
    }
}

/// Media type of a raw-bytes body.
const OCTET_STREAM: &str = "application/octet-stream";
/// Media type of an `Html<T>` body.
const TEXT_HTML: &str = "text/html";
/// Media type of an `Sse<S>` body.
const EVENT_STREAM: &str = "text/event-stream";
/// Media type of a bare string body.
const TEXT_PLAIN: &str = "text/plain";

/// True for raw byte bodies axum sends as `application/octet-stream`:
/// `Bytes`, `BytesMut`, `Body`, `Vec<u8>`, `Box<[u8]>`, `Cow<'static, [u8]>`,
/// `[u8; N]`, `&[u8]` and `&[u8; N]`.
fn is_bytes_like(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => is_byte_buffer(&reference.elem),
        Type::Array(array) => is_u8(&array.elem),
        Type::Path(type_path) => {
            type_path
                .path
                .segments
                .last()
                .is_some_and(|seg| match seg.ident.to_string().as_str() {
                    "Bytes" | "BytesMut" | "Body" => true,
                    "Vec" => type_arg_of(ty, "Vec").is_some_and(is_u8),
                    "Box" | "Cow" => crate::schema_macro::type_utils::first_generic_type_arg(seg)
                        .is_some_and(is_byte_buffer),
                    _ => false,
                })
        }
        _ => false,
    }
}

/// `[u8]` or `[u8; N]`.
fn is_byte_buffer(ty: &Type) -> bool {
    match ty {
        Type::Slice(slice) => is_u8(&slice.elem),
        Type::Array(array) => is_u8(&array.elem),
        _ => false,
    }
}

fn is_u8(ty: &Type) -> bool {
    matches!(ty, Type::Path(type_path) if type_path.path.is_ident("u8"))
}

/// True for bodies whose shape the signature does not reveal: `impl
/// IntoResponse` (any `impl Trait`), a type-erased `Response`, axum's
/// `ErrorResponse`, a `Redirect` and `NoContent` (no body at all). They are
/// documented by description only.
fn is_opaque_body(ty: &Type) -> bool {
    match ty {
        Type::ImplTrait(_) => true,
        Type::Path(type_path) => type_path.path.segments.last().is_some_and(|seg| {
            ["Response", "Redirect", "ErrorResponse", "NoContent"]
                .iter()
                .any(|ident| seg.ident == ident)
        }),
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
        Some(TEXT_PLAIN)
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

/// The original `(Ok, Err)` argument types of a `Result<Ok, Err>` return type
/// (no `Json` unwrapping). `Err` is `None` for axum's one-argument
/// `Result<T>`, whose error (`ErrorResponse`) is opaque.
fn result_args(ty: &Type) -> Option<(&Type, Option<&Type>)> {
    let mut inner = unwrap_json(ty);
    if let Type::Reference(type_ref) = inner {
        inner = type_ref.elem.as_ref();
    }
    if let Type::Path(type_path) = inner
        && is_keyword_type_by_type_path(type_path, &KeywordType::Result)
        && let Some(segment) = type_path.path.segments.last()
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
    {
        let mut types = args.args.iter().filter_map(|arg| match arg {
            syn::GenericArgument::Type(ty) => Some(ty),
            _ => None,
        });
        return types.next().map(|ok| (ok, types.next()));
    }
    None
}

/// Wrapper / response idents [`parse_return_type`] understands beyond the
/// schema builtins (see [`is_builtin_openapi_type`]).
const RESPONSE_IDENTS: [&str; 13] = [
    "Json",
    "Html",
    "Sse",
    "Bytes",
    "BytesMut",
    "Body",
    "Response",
    "Redirect",
    "NoContent",
    "ErrorResponse",
    "HeaderMap",
    "CookieJar",
    "Value",
];

/// True for a named body type whose shape vespera cannot resolve: not a
/// builtin, not a response wrapper, and not a known `Schema` type (an
/// unexpanded alias, a custom `IntoResponse` without `#[derive(Schema)]`).
/// Its wire shape is unknown, so it is documented by description only
/// instead of claiming a generic JSON object.
fn is_unresolved_named_type(
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> bool {
    crate::schema_macro::type_utils::last_path_segment(ty).is_some_and(|segment| {
        let ident = segment.ident.to_string();
        !(is_builtin_openapi_type(&ident)
            || RESPONSE_IDENTS.contains(&ident.as_str())
            || known_schemas.contains(ident.as_str())
            || struct_definitions.contains_key(ident.as_str()))
    })
}

fn content_for_type(
    ty: &Type,
    content_type: Option<&str>,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<BTreeMap<String, MediaType>> {
    let content_type = content_type?;
    // Text / Html / Sse / raw-byte bodies are strings on the wire regardless
    // of the Rust payload type; bytes are documented as `format: binary`.
    let schema = match content_type {
        TEXT_PLAIN | TEXT_HTML | EVENT_STREAM => SchemaRef::Inline(Box::new(Schema::string())),
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
            encoding: None,
        },
    );
    Some(content)
}

/// The documented content of a response type's body (see [`body_element`]),
/// or `None` for no / an opaque / an unresolved body.
fn response_content(
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<BTreeMap<String, MediaType>> {
    let body = body_element(ty)?;
    if is_unresolved_named_type(body, known_schemas, struct_definitions) {
        return None;
    }
    content_for_type(
        unwrap_json(body),
        body_content_type(body),
        known_schemas,
        struct_definitions,
    )
}

/// The status key of a successful response: `204` for axum's `NoContent`,
/// the `3XX` range for a `Redirect` (its constructor — `to` 303, `temporary`
/// 307, `permanent` 308 — is not visible in the signature), else `200`.
fn success_key(ty: &Type) -> &'static str {
    match body_element(ty) {
        Some(body) if last_ident_is(body, "NoContent") => "204",
        Some(body) if last_ident_is(body, "Redirect") => "3XX",
        _ => "200",
    }
}

fn insert_success_response(
    responses: &mut BTreeMap<String, Response>,
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) {
    let key = success_key(ty);
    responses.insert(
        key.to_string(),
        Response {
            description: if key == "3XX" {
                "Redirect".to_string()
            } else {
                "Successful response".to_string()
            },
            headers: has_header_map(ty).then(BTreeMap::new),
            content: response_content(ty, known_schemas, struct_definitions),
        },
    );
}

/// Analyze return type and convert to Responses map
pub fn parse_return_type(
    return_type: &ReturnType,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> BTreeMap<String, Response> {
    let mut responses = BTreeMap::new();
    let ReturnType::Type(_, ty) = return_type else {
        // No return type - just 200 with no content
        insert_success_response(
            &mut responses,
            &syn::parse_quote!(()),
            known_schemas,
            struct_definitions,
        );
        return responses;
    };
    if let Some((ok_ty, err_ty)) = extract_result_types(ty) {
        insert_success_response(&mut responses, &ok_ty, known_schemas, struct_definitions);
        // The error body is found like the success one: `(StatusCode,
        // Json<E>)`, `(HeaderMap, Json<E>)` and header arrays document `E`; a
        // metadata-only error (`StatusCode`, `(StatusCode, HeaderMap)`) has
        // no content. The default error status is `400`.
        responses.insert(
            "400".to_string(),
            Response {
                description: "Error response".to_string(),
                headers: None,
                content: response_content(&err_ty, known_schemas, struct_definitions),
            },
        );
    } else {
        insert_success_response(&mut responses, ty, known_schemas, struct_definitions);
    }
    responses
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_parts;
