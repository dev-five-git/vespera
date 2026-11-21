use std::collections::{BTreeMap, HashMap, HashSet};

use syn::{ReturnType, Type};
use vespera_core::route::{MediaType, Response};
use vespera_core::schema::{Schema, SchemaRef};

use super::schema::{is_builtin_openapi_type, parse_type_to_schema_ref};
use crate::parser::is_keyword_type::{KeywordType, is_keyword_type, is_keyword_type_by_type_path};

/// Unwrap Json<T> to get T
/// Handles both Json<T> and `vespera::axum::Json`<T> by checking the last segment.
/// A user type named `Json` is not the wrapper and is returned as written.
fn unwrap_json<'a>(ty: &'a Type, shadows: Shadows<'_>) -> &'a Type {
    // Check the last segment (handles both `Json<T>` and
    // `vespera::axum::Json<T>`). `segments.last()` is `None` for an empty
    // path, so the let-chain replaces the prior `is_empty()` guard + `unwrap()`.
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
        && segment.ident == "Json"
        && !shadows.names_user_type(ty)
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
#[cfg(test)]
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

/// Path roots naming an external crate (or the `vespera` re-export of axum): a
/// path rooted there cannot name one of this crate's own types.
const EXTERNAL_ROOTS: [&str; 6] = ["axum", "axum_extra", "http", "std", "core", "vespera"];

/// The types this crate defines (known `Schema` types and struct definitions).
/// A user type named like an axum wrapper shadows the wrapper by ident, so the
/// wrapper checks consult these, except for paths rooted at an external crate.
#[derive(Clone, Copy)]
pub(super) struct Shadows<'a> {
    pub(super) known_schemas: &'a HashSet<&'a str>,
    pub(super) struct_definitions: &'a HashMap<&'a str, &'a str>,
}

impl Shadows<'_> {
    /// Whether `ty`'s last path segment names a type this crate defines (a
    /// known `Schema` type or a struct definition). A path whose first segment
    /// is an external root (`axum::response::Redirect`) names the external type.
    pub(super) fn names_user_type(self, ty: &Type) -> bool {
        let Type::Path(type_path) = ty else {
            return false;
        };
        let segments = &type_path.path.segments;
        let external = segments
            .first()
            .is_some_and(|first| EXTERNAL_ROOTS.contains(&first.ident.to_string().as_str()));
        !external
            && segments.last().is_some_and(|segment| {
                let ident = segment.ident.to_string();
                self.known_schemas.contains(ident.as_str())
                    || self.struct_definitions.contains_key(ident.as_str())
            })
    }
}

/// Check if a type is a non-body response type (metadata only).
/// These types contribute to the HTTP response (status, headers, cookies,
/// extensions) but do not form the response body.
/// `[(HeaderName, &str); N]` header arrays are axum `IntoResponseParts` too.
/// A bare `()` is axum's empty response (no body at all), unlike `Json<()>`,
/// which serializes `null`.
fn is_non_body_type(ty: &Type, shadows: Shadows<'_>) -> bool {
    is_unit(ty)
        || matches!(ty, Type::Array(array) if matches!(array.elem.as_ref(), Type::Tuple(_)))
        || (!shadows.names_user_type(ty)
            && (is_keyword_type(ty, &KeywordType::StatusCode)
                || is_keyword_type(ty, &KeywordType::HeaderMap)
                || is_keyword_type(ty, &KeywordType::CookieJar)
                || RESPONSE_PART_IDENTS
                    .iter()
                    .any(|ident| last_ident_is(ty, ident))))
}

/// The element of a response type that forms the body, as written (`Json`
/// not unwrapped): the last non-metadata element of a tuple
/// (`(StatusCode, HeaderMap, Json<T>)` → `Json<T>`), `None` when the type is
/// metadata only (`()`, `StatusCode`, `HeaderMap`, `(StatusCode, CookieJar)`),
/// otherwise the type itself. A nested response tuple (`(StatusCode,
/// (HeaderMap, Json<T>))` — the last element only has to be
/// `IntoResponse`) is searched the same way, and so is the `Ok` side of a
/// `Result` (axum's `IntoResponse for Result` sends `T` or `E`'s response,
/// never serde's `{"Ok": ..}`; the errors are [`result_errors`]).
fn body_element<'a>(ty: &'a Type, shadows: Shadows<'_>) -> Option<&'a Type> {
    match ty {
        Type::Tuple(tuple) => body_element(
            tuple
                .elems
                .iter()
                .rev()
                .find(|elem| !is_non_body_type(elem, shadows))?,
            shadows,
        ),
        _ if is_non_body_type(ty, shadows) => None,
        _ => result_args(ty).map_or(Some(ty), |(ok, _)| body_element(ok, shadows)),
    }
}

/// The error types of the `Result`s on the body path of `ty`
/// (`Result<Result<Json<T>, E1>, E2>`, `(StatusCode, Result<Json<T>, E>)`),
/// outermost first: each is an error response.
fn result_errors(ty: &Type, shadows: Shadows<'_>) -> Vec<Type> {
    match ty {
        Type::Tuple(tuple) => tuple
            .elems
            .iter()
            .rev()
            .find(|elem| !is_non_body_type(elem, shadows))
            .map_or_else(Vec::new, |elem| result_errors(elem, shadows)),
        _ => result_args(ty).map_or_else(Vec::new, |(ok, err)| {
            let mut errors = vec![err.cloned().unwrap_or_else(axum_error_response)];
            errors.extend(result_errors(ok, shadows));
            errors
        }),
    }
}

/// Whether a response type sets headers through a `HeaderMap` (bare or as a
/// tuple element, nested tuples and a `Result`'s `Ok` side included).
fn has_header_map(ty: &Type, shadows: Shadows<'_>) -> bool {
    match ty {
        Type::Tuple(tuple) => tuple.elems.iter().any(|elem| has_header_map(elem, shadows)),
        _ => result_args(ty).map_or_else(
            || is_keyword_type(ty, &KeywordType::HeaderMap) && !shadows.names_user_type(ty),
            |(ok, _)| has_header_map(ok, shadows),
        ),
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
/// `Bytes`, `BytesMut`, `Vec<u8>`, `Box<[u8]>`, `Cow<'static, [u8]>`,
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
                    "Bytes" | "BytesMut" => true,
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
/// `ErrorResponse`, a `Redirect` and `NoContent` (no body at all), and a bare
/// `Body` (axum sets no `Content-Type` for it, so none can be claimed). They
/// are documented by description only. A user type of one of those names (a
/// known schema or a struct definition) is not axum's, so it is not opaque;
/// a path rooted at an external crate (`axum::response::Redirect`) always is.
fn is_opaque_body(ty: &Type, shadows: Shadows<'_>) -> bool {
    match ty {
        Type::ImplTrait(_) => true,
        Type::Path(type_path) => {
            type_path.path.segments.last().is_some_and(|seg| {
                ["Response", "Redirect", "ErrorResponse", "NoContent", "Body"]
                    .iter()
                    .any(|ident| seg.ident == ident)
            }) && !shadows.names_user_type(ty)
        }
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
fn body_content_type(ty: &Type, shadows: Shadows<'_>) -> Option<&'static str> {
    if is_opaque_body(ty, shadows) {
        None
    } else if is_string_like(ty) {
        Some(TEXT_PLAIN)
    } else if last_ident_is(ty, "Html") && !shadows.names_user_type(ty) {
        Some(TEXT_HTML)
    } else if last_ident_is(ty, "Sse") && !shadows.names_user_type(ty) {
        Some(EVENT_STREAM)
    } else if is_bytes_like(ty) && !shadows.names_user_type(ty) {
        Some(OCTET_STREAM)
    } else {
        Some("application/json")
    }
}

/// The original `(Ok, Err)` argument types of a `Result<Ok, Err>` return type
/// (no `Json` unwrapping). `Err` is `None` for axum's one-argument
/// `Result<T>`, whose error (`ErrorResponse`) is opaque.
fn result_args(ty: &Type) -> Option<(&Type, Option<&Type>)> {
    // `Json<Result<A, B>>` is NOT a result: it is one `200` body, serde's
    // externally tagged `{"Ok": A}` / `{"Err": B}` (see the type schema).
    let mut inner = ty;
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
/// Its wire shape is unknown, so a bare such body is documented by description
/// only instead of claiming a generic JSON object. `Json<Unknown>` is not
/// one: `Json` is the wrapper, so it keeps `application/json` with the
/// unresolved payload's schema (`{}`, any value).
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
    let shadows = Shadows {
        known_schemas,
        struct_definitions,
    };
    let body = body_element(ty, shadows)?;
    if is_unresolved_named_type(body, known_schemas, struct_definitions) {
        return None;
    }
    content_for_type(
        unwrap_json(body, shadows),
        body_content_type(body, shadows),
        known_schemas,
        struct_definitions,
    )
}

/// The status key of a successful response: `204` for axum's `NoContent`,
/// the `3XX` range for a `Redirect` (its constructor — `to` 303, `temporary`
/// 307, `permanent` 308 — is not visible in the signature), else `200`.
fn success_key(
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> &'static str {
    let shadows = Shadows {
        known_schemas,
        struct_definitions,
    };
    let axum_ident =
        |body: &Type, ident: &str| last_ident_is(body, ident) && !shadows.names_user_type(body);
    match body_element(ty, shadows) {
        Some(body) if axum_ident(body, "NoContent") => "204",
        Some(body) if axum_ident(body, "Redirect") => "3XX",
        _ => "200",
    }
}

/// `std::convert::Infallible` (by its last segment) or the never type `!`:
/// an error type no value inhabits.
fn is_infallible(ty: &Type) -> bool {
    matches!(ty, Type::Never(_)) || last_ident_is(ty, "Infallible")
}

fn insert_success_response(
    responses: &mut BTreeMap<String, Response>,
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) {
    let shadows = Shadows {
        known_schemas,
        struct_definitions,
    };
    let key = success_key(ty, known_schemas, struct_definitions);
    responses.insert(
        key.to_string(),
        Response {
            description: if key == "3XX" {
                "Redirect".to_string()
            } else {
                "Successful response".to_string()
            },
            headers: has_header_map(ty, shadows).then(BTreeMap::new),
            content: response_content(ty, known_schemas, struct_definitions),
            ..Default::default()
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
    // `Result<T, E>`: `T` is the success, `E` (and the error of any `Result`
    // nested in `T`) an error response.
    insert_success_response(&mut responses, ty, known_schemas, struct_definitions);
    let shadows = Shadows {
        known_schemas,
        struct_definitions,
    };
    for err_ty in result_errors(ty, shadows) {
        insert_error_response(&mut responses, &err_ty, known_schemas, struct_definitions);
    }
    responses
}

/// Document the error type `err_ty` of a `Result` return type.
///
/// The error body is found like the success one: `(StatusCode, Json<E>)`,
/// `(HeaderMap, Json<E>)` and header arrays document `E`; a metadata-only
/// error (`StatusCode`, `(StatusCode, HeaderMap)`) has no content. A
/// `HeaderMap` the error sets (bare or in the tuple) documents `headers` on
/// its status, as the success side does. The default error status is `400`;
/// an error that is a `Redirect` /
/// `NoContent` is keyed like the success side (`3XX` / `204`), never
/// overwriting the success entry. An error documented without a body (an
/// outer `StatusCode`) takes the body of a later (inner) one, and several
/// bodies on one status are merged with `oneOf` ([`merge_error_bodies`]).
fn insert_error_response(
    responses: &mut BTreeMap<String, Response>,
    err_ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) {
    // `Result<T, Infallible>` / `Result<T, !>` never takes the error side.
    if is_infallible(err_ty) {
        return;
    }
    let key = match success_key(err_ty, known_schemas, struct_definitions) {
        "200" => "400",
        key => key,
    };
    let shadows = Shadows {
        known_schemas,
        struct_definitions,
    };
    let response = responses
        .entry(key.to_string())
        .or_insert_with(|| Response {
            // Labelled by the status it is keyed under: a `NoContent` error is
            // still a 2xx.
            description: match key {
                "3XX" => "Redirect",
                "204" => "Successful response",
                _ => "Error response",
            }
            .to_string(),
            headers: None,
            content: None,
            ..Default::default()
        });
    response.headers = response
        .headers
        .take()
        .or_else(|| has_header_map(err_ty, shadows).then(BTreeMap::new));
    let Some(content) = response_content(err_ty, known_schemas, struct_definitions) else {
        return;
    };
    match &mut response.content {
        None => response.content = Some(content),
        Some(existing) => merge_error_bodies(existing, content),
    }
}

/// Add the body `later` of another error that shares a status key with
/// `existing`. Two bodies of one status are alternatives, so a media type
/// both document is `oneOf` of the two (an identical body is kept once).
fn merge_error_bodies(
    existing: &mut BTreeMap<String, MediaType>,
    later: BTreeMap<String, MediaType>,
) {
    for (media, later) in later {
        let Some(earlier) = existing.get_mut(&media) else {
            existing.insert(media, later);
            continue;
        };
        earlier.schema = match (earlier.schema.take(), later.schema) {
            (Some(first), Some(second))
                if serde_json::to_string(&first).ok() != serde_json::to_string(&second).ok() =>
            {
                Some(SchemaRef::Inline(Box::new(Schema {
                    one_of: Some(vec![first, second]),
                    ..Schema::default()
                })))
            }
            (first, second) => first.or(second),
        };
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_parts;
