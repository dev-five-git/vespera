//! Internal dispatch plumbing shared by every public entry point:
//! request building, router oneshot driving, and response collection.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use axum::body::Body;
use bytes::Bytes;
use http::{HeaderName, Method, Request, Uri, header::CONTENT_TYPE};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::Router;
use crate::envelope::{HeaderValue, ResponseEnvelope, ResponseMetadata};

// ── Internal Helpers ─────────────────────────────────────────────────

/// Raw response parts on the wire path.  Headers stay as the owned
/// [`http::HeaderMap`] taken from `Response::into_parts` — zero
/// per-header allocation; conversion to the public
/// `BTreeMap<String, HeaderValue>` shape happens only on the text
/// envelope path ([`to_response_envelope_text`]).
pub type ResponseParts = (u16, http::HeaderMap, Bytes, ResponseMetadata);

/// Parse the wire `method` string into an [`http::Method`], surfacing the
/// invalid-method failure as the canonical `405 Method Not Allowed` wire
/// error.  The two owned-wire entry points
/// ([`build_request_from_bytes`] and [`dispatch_and_split`]) used to inline
/// this exact 6-line block; a single-site edit to the error string would
/// have silently drifted the other path — see the prior
/// `tests/method_validation.rs` golden, which exercises BOTH paths.
/// `#[inline]` so the helper folds back into each caller, keeping codegen
/// byte-identical to the prior copy-pasted shape.
#[inline]
fn parse_method_or_405(method_str: &str) -> Result<Method, (u16, String)> {
    method_str.parse::<Method>().map_err(|_| {
        (
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        )
    })
}

/// Build an [`http::HeaderValue`] from a wire-borrowed value string,
/// sharing bytes with the request's owning `header_bytes` when the value
/// lies inside it.
///
/// `HeaderValue::from_maybe_shared` specializes on `Bytes` via the http
/// crate's `if_downcast_into!(T, Bytes, ...)` and stores the slice with
/// **zero copy** when the input is a [`bytes::Bytes`]; the byte-range
/// validation is byte-identical to [`http::HeaderValue::from_str`], so an
/// invalid value still yields the same `InvalidHeaderValue` error (the
/// 400 wire-response path is preserved).  When `owner` is `None`, or
/// when `value` does not lie inside `owner` (an escaped `Cow::Owned`, an
/// envelope-path header on the non-wire dispatch, …), this falls back to
/// the copying `from_str` exactly as before — same bytes, same errors.
///
/// `slice_from_owner` already guards provenance: a returned `Some(bytes)`
/// is the exact sub-`Bytes` of `owner` covering `value`, so the
/// constructed `HeaderValue` shares the wire allocation and the
/// allocation stays alive via `Bytes`' refcount for the request's
/// lifetime (axum drops the request, the `HeaderValue`s drop, the
/// refcount falls to zero, the wire buffer is freed — exactly as today).
#[inline]
fn header_value_from_owner(
    value: &str,
    owner: Option<&Bytes>,
) -> Result<http::HeaderValue, http::header::InvalidHeaderValue> {
    if let Some(owner) = owner
        && let Some(slice) = crate::dispatch::slice_from_owner(owner, value)
    {
        http::HeaderValue::from_maybe_shared(slice)
    } else {
        http::HeaderValue::from_str(value)
    }
}

/// Drive a [`Router`] with the request and return the resulting response.
///
/// Carries the axum `Service<_, Error = Infallible>` contract once: the `Err`
/// variant is uninhabited, so the `match err {}` is exhaustive and emits
/// **no panic/unwind site on this FFI-adjacent hot path**.  Used by every
/// dispatcher in this module ([`dispatch_parts`], [`dispatch_response_streaming`],
/// [`dispatch_and_split`]).  `#[inline]` so the state machine collapses into
/// each caller exactly as the prior copy-pasted `match` shape did.
#[inline]
async fn router_oneshot(router: Router, request: Request<Body>) -> axum::response::Response {
    match router.oneshot(request).await {
        Ok(response) => response,
        // axum routers are `Service<_, Error = Infallible>`; the `Err`
        // variant is uninhabited, so this match is exhaustive and emits
        // no panic/unwind site on this FFI-adjacent hot path.
        Err(err) => match err {},
    }
}

/// Drive a [`Router`] with the supplied envelope fields and return
/// raw response parts.
///
/// Returns `Err((status, msg))` only for pre-dispatch errors
/// (currently only "invalid HTTP method" → 405).  Router/handler
/// errors cannot occur because axum routers are
/// `Service<_, Error = Infallible>`.
///
/// `header_bytes_owner` is the request's owning wire `Bytes` (the wire
/// header-JSON region) when the headers are borrowed slices of it; this
/// enables zero-copy `HeaderValue` construction via
/// [`header_value_from_owner`].  Pass `None` on non-wire paths (envelope
/// dispatch) where the header strings are owned and unrelated to any
/// `Bytes` allocation — the helper falls back to the existing copy path.
pub async fn dispatch_parts<'h>(
    router: Router,
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body_bytes: Bytes,
    header_bytes_owner: Option<&Bytes>,
) -> Result<ResponseParts, (u16, String)> {
    let request = build_request_from_bytes(
        method_str,
        path,
        query,
        headers,
        body_bytes,
        header_bytes_owner,
    )?;

    let response = router_oneshot(router, request).await;

    collect_response_parts(response).await
}

/// Start a request builder with method + URI.  When `query` is empty
/// the borrowed `path` feeds `Uri` parsing directly — no intermediate
/// `String`; otherwise a single exact-capacity join is allocated.
#[cfg(any(test, feature = "bench-support"))]
fn request_builder(method: Method, path: &str, query: &str) -> http::request::Builder {
    let builder = Request::builder().method(method);
    if query.is_empty() {
        builder.uri(path)
    } else {
        let mut uri = String::with_capacity(path.len() + 1 + query.len());
        uri.push_str(path);
        uri.push('?');
        uri.push_str(query);
        builder.uri(uri)
    }
}

/// Parse the request [`Uri`] from `path` (+ optional `query`), mirroring
/// [`request_builder`]'s borrowed-path optimization: an empty query parses
/// `path` directly (no intermediate `String`); otherwise a single
/// exact-capacity join is allocated.  A malformed path/query that `http`
/// rejects becomes `Err((400, _))`, upholding the "every failure returns a
/// wire response" contract.
fn build_uri(path: &str, query: &str) -> Result<Uri, (u16, String)> {
    let parsed = if query.is_empty() {
        Uri::try_from(path)
    } else {
        let mut uri = String::with_capacity(path.len() + 1 + query.len());
        uri.push_str(path);
        uri.push('?');
        uri.push_str(query);
        Uri::try_from(uri)
    };
    parsed.map_err(|e| (400, format!("invalid request: {e}")))
}

/// Shared inner request build for [`build_request_from_bytes`] (the buffered /
/// response-streaming paths) and [`dispatch_and_split`] (the wire / streaming
/// / direct-write paths).  Both call sites used to inline the same
/// `Request::new(body)` → `method_mut()` / `uri_mut()` → `headers.reserve` →
/// single-pass header insert with case-insensitive `Content-Type` detection →
/// default-`application/json` sequence; this helper carries that sequence once.
///
/// Constructs the [`Request`] **directly** — `Request::new(body)` then
/// in-place method / URI / header assignment — instead of threading the
/// `http::request::Builder` state machine, which re-checks an internal
/// `Result<Parts>` and is moved by value on every `.method`/`.uri`/`.header`
/// call.  The `HeaderMap` is pre-reserved from the header `size_hint().0`
/// plus the possible default-content-type slot, so header insertion never
/// triggers an incremental grow; a bodyless, headerless request without a
/// default reserves `0` and never allocates a bucket (preserving the
/// DIRECT-`GET` zero-allocation sweet spot).  Header names/values are parsed
/// with `HeaderName::from_bytes` / `header_value_from_owner` (the same parsers
/// each call site used) and are `append`ed (not `insert`ed), preserving
/// duplicate-name multi-value semantics byte-for-byte.  Case-insensitive
/// `Content-Type` detection (RFC 7230 §3.2) is tracked inside the single
/// insertion pass — `HeaderName::from_bytes` already ASCII-lowercased the
/// name, so the `== CONTENT_TYPE` standard-header comparison is the same
/// cheap discriminant compare each site already used.  `default_json_when_absent`
/// requests the `application/json` default when no `Content-Type` was seen.
/// Returns `Err((400, _))` for a malformed header, upholding the "every
/// failure returns a wire response" contract.  `#[inline]` so both call sites
/// keep the same inlined codegen as the prior copy-pasted shape.
#[inline]
fn build_axum_request_inner<'h>(
    http_method: Method,
    uri: Uri,
    body: Body,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    header_bytes_owner: Option<&Bytes>,
    default_json_when_absent: bool,
) -> Result<Request<Body>, (u16, String)> {
    let mut request = Request::new(body);
    *request.method_mut() = http_method;
    *request.uri_mut() = uri;

    let reserve = headers
        .size_hint()
        .0
        .saturating_add(usize::from(default_json_when_absent));
    let header_map = request.headers_mut();
    if reserve > 0 {
        header_map.reserve(reserve);
    }

    let mut has_content_type = false;
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| (400, format!("invalid request: {e}")))?;
        let header_value = header_value_from_owner(value, header_bytes_owner)
            .map_err(|e| (400, format!("invalid request: {e}")))?;
        has_content_type = has_content_type || header_name == CONTENT_TYPE;
        header_map.append(header_name, header_value);
    }
    if default_json_when_absent && !has_content_type {
        header_map.append(
            CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
    }
    Ok(request)
}

/// Build the axum request shared by the buffered ([`dispatch_parts`]) and
/// response-streaming ([`dispatch_response_streaming`]) paths — both take a
/// fully-buffered [`Bytes`] body and default a missing `Content-Type` when
/// the body is non-empty.  Parses the method (`Err((405, _))` on failure) and
/// the URI (`Err((400, _))` on failure), then delegates the in-place
/// `Request` construction to [`build_axum_request_inner`].  `#[inline]` so
/// the two call sites keep inlined codegen.
#[inline]
fn build_request_from_bytes<'h>(
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body_bytes: Bytes,
    header_bytes_owner: Option<&Bytes>,
) -> Result<Request<Body>, (u16, String)> {
    let http_method = parse_method_or_405(method_str)?;
    let uri = build_uri(path, query)?;
    let body_is_empty = body_bytes.is_empty();
    build_axum_request_inner(
        http_method,
        uri,
        Body::from(body_bytes),
        headers,
        header_bytes_owner,
        !body_is_empty,
    )
}

/// **Bench-only** `http::request::Builder` twin of
/// [`build_request_from_bytes`], retained solely as the "before" arm of the
/// `request_build_ab` criterion A/B (same-run, noise-robust — mirroring the
/// `wire_header_serde` group's hand-vs-`serde_json` twin).  Routes the request
/// through the builder state machine the production path replaced; produces a
/// byte-identical request.  Not used on any production path.
#[cfg(any(test, feature = "bench-support"))]
fn build_request_from_bytes_builder_old<'h>(
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body_bytes: Bytes,
) -> Result<Request<Body>, (u16, String)> {
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };
    let mut builder = request_builder(http_method, path, query);
    let mut has_content_type = false;
    for (name, value) in headers {
        has_content_type = has_content_type || name.eq_ignore_ascii_case("content-type");
        builder = builder.header(name, value);
    }
    if !body_bytes.is_empty() && !has_content_type {
        builder = builder.header("content-type", "application/json");
    }
    builder
        .body(Body::from(body_bytes))
        .map_err(|e| (400, format!("invalid request: {e}")))
}

/// Sum a built request's method / path / query / header byte lengths so the
/// `request_build_ab` A/B cannot be optimised down to a partial build.
/// Bench-only.
#[cfg(any(test, feature = "bench-support"))]
fn request_field_len_sum(req: &Request<Body>) -> usize {
    let mut acc = req.method().as_str().len() + req.uri().path().len();
    if let Some(query) = req.uri().query() {
        acc += query.len();
    }
    for (name, value) in req.headers() {
        acc += name.as_str().len() + value.len();
    }
    acc
}

/// Bench A/B: production direct-construction request build cost.  Returns a
/// summed length so the optimiser cannot elide the build.  Bench-only.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_build_request_new(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> usize {
    // Bench A/B: no wire-owning `Bytes` is plumbed through this surface
    // (the bench measures the request-build cost in isolation, not the
    // wire prelude), so pass `None` — the helper falls back to the same
    // `from_str` copy path the old shape used, keeping the A/B fair.
    build_request_from_bytes(method, path, query, headers.iter().copied(), body, None)
        .map_or(usize::MAX, |req| request_field_len_sum(&req))
}

/// Bench A/B: previous `http::request::Builder` request build cost.
/// Bench-only.
#[cfg(any(test, feature = "bench-support"))]
#[doc(hidden)]
#[must_use]
pub fn bench_build_request_old(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> usize {
    build_request_from_bytes_builder_old(method, path, query, headers.iter().copied(), body)
        .map_or(usize::MAX, |req| request_field_len_sum(&req))
}

/// Drive a [`Router`] and stream response body chunks through
/// `on_chunk`, returning the status/headers/metadata once the body
/// stream finishes.
///
/// Same pre-dispatch error semantics as [`dispatch_parts`] (invalid
/// HTTP method → `Err((405, ...))`).  A **response body stream error**
/// mid-drain returns `Err((500, ...))` so the caller emits a 500 wire
/// response instead of reporting the partially-streamed body as a
/// success — a truncated body must never be presented as complete.
/// (Chunks emitted via `on_chunk` before the error have already left,
/// but the 500 status the caller returns signals the failure.)
// 8 params: the request line (method / path / query), the borrowed header
// iterator, the body, the optional wire-`Bytes` owner for zero-copy
// `HeaderValue` construction, and the chunk sink are each distinct per-request
// inputs.  Bundling them into a struct would add indirection on this hot path
// without removing any genuinely-needed data — same reasoning as
// [`dispatch_and_split`].
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_response_streaming<'h, F>(
    router: Router,
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body_bytes: Bytes,
    header_bytes_owner: Option<&Bytes>,
    on_chunk: &mut F,
) -> Result<(u16, http::HeaderMap, ResponseMetadata), (u16, String)>
where
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    let request = build_request_from_bytes(
        method_str,
        path,
        query,
        headers,
        body_bytes,
        header_bytes_owner,
    )?;

    let response = router_oneshot(router, request).await;

    let (parts, mut body) = response.into_parts();

    // Stream body chunks: pull frames one at a time and surface only
    // data frames (trailers are dropped — wire format does not carry
    // them).  A frame error means the body aborted mid-stream; propagate
    // it as a 500 so a truncated response is never reported as a clean
    // success.
    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref()
                    && !data.is_empty()
                    && on_chunk(data.as_ref()).is_break()
                {
                    // The chunk sink asked to stop EARLY (e.g. the host's
                    // OutputStream failed mid-stream).  The bytes already
                    // delivered are truncated, so surface a 500 — exactly
                    // like the body-error arm below — instead of falling
                    // through to the original success header, which would
                    // report a short, truncated response as a clean success.
                    return Err((
                        500,
                        "response body sink stopped before completion".to_owned(),
                    ));
                }
            }
            Some(Err(_)) => {
                return Err((500, "response body stream error".to_owned()));
            }
            None => break,
        }
    }

    Ok((
        parts.status.as_u16(),
        parts.headers,
        ResponseMetadata::current(),
    ))
}

/// Collapse an [`http::HeaderMap`] into the wire's name → value map.
/// Headers with repeated names (e.g. `set-cookie`) are preserved as
/// [`HeaderValue::Multi`] so their semantics survive the conversion.
fn collect_header_map(headers: &http::HeaderMap) -> BTreeMap<String, HeaderValue> {
    let mut resp_headers: BTreeMap<String, HeaderValue> = BTreeMap::new();
    for (name, value) in headers {
        let val_str = value.to_str().unwrap_or("").to_owned();
        let name_str = name.as_str();
        // Split the lookup so the owned key (`name_str.to_owned()`) is only
        // allocated on the Vacant insert branch. The previous
        // `entry(name.as_str().to_owned())` shape allocated a fresh `String`
        // key on EVERY iteration even when the entry turned out to be
        // Occupied (e.g. repeated `set-cookie`), where the new key was
        // dropped immediately — N-1 wasted allocs per N-valued name.
        if let Some(existing) = resp_headers.get_mut(name_str) {
            match existing {
                HeaderValue::Multi(v) => v.push(val_str),
                HeaderValue::Single(prev_str) => {
                    // Take ownership of the existing single value, then
                    // overwrite the slot with the new Multi.  Final state
                    // is byte-identical to the prior `mem::replace` +
                    // `unreachable!()` form, but with no panic landing pad.
                    let prev = std::mem::take(prev_str);
                    *existing = HeaderValue::Multi(vec![prev, val_str]);
                }
            }
        } else {
            resp_headers.insert(name_str.to_owned(), HeaderValue::Single(val_str));
        }
    }
    resp_headers
}

/// Collect status, headers, body bytes, and metadata from an axum
/// response.  Headers with repeated names are collapsed into
/// [`HeaderValue::Multi`] so semantics (e.g. `set-cookie`) are
/// preserved.
///
/// A body-stream error while collecting returns `Err((500, _))` instead
/// of silently yielding an empty body — a truncated/failed response must
/// never be reported as a clean success.  This mirrors the
/// response-streaming path ([`dispatch_response_streaming`]), which
/// already surfaces mid-stream body errors as a 500.
async fn collect_response_parts(
    response: axum::response::Response,
) -> Result<ResponseParts, (u16, String)> {
    let (parts, body) = response.into_parts();

    let body_bytes = body
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .map_err(|_| (500u16, "response body stream error".to_owned()))?;

    Ok((
        parts.status.as_u16(),
        parts.headers,
        body_bytes,
        ResponseMetadata::current(),
    ))
}

/// Adapter: response parts → text envelope.  Non-UTF-8 bodies become
/// the empty string.  The owned-`String` header conversion happens
/// only here — the wire path serializes straight from the
/// [`http::HeaderMap`].
pub fn to_response_envelope_text(parts: ResponseParts) -> ResponseEnvelope {
    let (status, headers, body_bytes, metadata) = parts;
    // `Vec::from(Bytes)` reuses the underlying buffer when the `Bytes`
    // is uniquely owned (the common case for a collected response body),
    // copying only for a shared/static slice — unlike `to_vec()`, which
    // always allocates and copies.  Semantics preserved: a non-UTF-8
    // body still yields the empty string.
    let body = String::from_utf8(Vec::from(body_bytes)).unwrap_or_default();
    ResponseEnvelope {
        status,
        headers: collect_header_map(&headers),
        body,
        metadata,
    }
}

/// Dispatch a request and split the response into
/// `(status, headers, metadata, body)` — exposing `axum::body::Body`
/// so callers can stream it themselves (vs. collecting it eagerly).
///
/// Used by the `*_with_header` streaming variants which need to emit
/// the wire-format header **before** body bytes start flowing.
///
/// `default_json_when_absent` requests `content-type: application/json`
/// defaulting (mirroring [`dispatch_parts`]'s defaulting).  This function
/// detects whether the caller's `headers` already carry a `Content-Type`
/// **during its single header-insertion pass** and appends the default
/// only when the flag is set AND none was present — folding in the
/// content-type detection each caller used to run as a separate pre-scan.
/// Callers that know the body is non-empty pass `!body.is_empty()`;
/// streaming callers whose body emptiness is unknowable up front pass
/// `true` (default whenever absent).
// 9 params: the request line (method / path / query / path_bytes /
// header_bytes_owner), the borrowed header iterator, the body, and the
// content-type-default flag are each distinct per-request inputs.  Bundling
// them into a struct would add indirection on this hot path without removing
// any genuinely-needed data.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_and_split<'h>(
    router: Router,
    method_str: &str,
    path: &str,
    query: &str,
    path_bytes: Option<Bytes>,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    header_bytes_owner: Option<&Bytes>,
    body: Body,
    default_json_when_absent: bool,
) -> Result<(u16, http::HeaderMap, ResponseMetadata, Body), (u16, String)> {
    let http_method = parse_method_or_405(method_str)?;
    // Same contract as dispatch_parts: a malformed path/header must surface as
    // a 400 wire response, not a panic.
    //
    // `path_bytes` is `Some` only on the OWNED wire path with an empty query
    // and a path whose bytes already live in the request's owning `Bytes`
    // (a borrowed `Cow` sliced from the wire header — see `slice_from_owner`).
    // Building the `Uri` by SHARING those bytes skips the `Bytes::copy_from_slice`
    // that `Uri::try_from(&str)` performs — one fewer per-request allocation.
    // The parsed URI is byte-identical (same origin-form/absolute parse as
    // `build_uri`); any owned/escaped path or non-empty query passes `None` and
    // falls back to the copying join.
    let uri = match path_bytes {
        Some(bytes) => {
            Uri::from_maybe_shared(bytes).map_err(|e| (400, format!("invalid request: {e}")))?
        }
        None => build_uri(path, query)?,
    };

    // Delegate the in-place `Request` construction / `HeaderMap` reserve /
    // single-pass header insert + case-insensitive `Content-Type` detection +
    // optional `application/json` default to the shared helper.  The body is
    // opaque here (already-built `Body`, not a `Bytes` we can probe for
    // emptiness), so content-type defaulting follows the caller's
    // `default_json_when_absent` flag rather than body-emptiness detection.
    let request = build_axum_request_inner(
        http_method,
        uri,
        body,
        headers,
        header_bytes_owner,
        default_json_when_absent,
    )?;

    let response = router_oneshot(router, request).await;

    let (parts, body) = response.into_parts();
    Ok((
        parts.status.as_u16(),
        parts.headers,
        ResponseMetadata::current(),
        body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime")
            .block_on(fut)
    }

    /// A wire `path` that cannot be parsed into an [`http::Uri`] (a raw
    /// space is illegal) must surface as an `Err((4xx, _))` the caller
    /// turns into a wire response — never a panic.  Guards the
    /// "all failure modes return a valid wire response" contract for
    /// every `request_builder` call site.
    #[test]
    fn malformed_path_returns_error_not_panic() {
        let result = block_on(async {
            dispatch_parts(
                crate::Router::new(),
                "GET",
                "bad path with spaces",
                "",
                std::iter::empty(),
                Bytes::new(),
                None,
            )
            .await
        });
        match result {
            Err((status, _)) => assert!(
                (400..500).contains(&status),
                "expected 4xx for malformed path, got {status}"
            ),
            Ok(_) => panic!("malformed path should not produce a successful dispatch"),
        }
    }

    #[test]
    fn malformed_path_streaming_returns_error_not_panic() {
        let result = block_on(async {
            let mut sink = |_: &[u8]| ControlFlow::Continue(());
            dispatch_response_streaming(
                crate::Router::new(),
                "GET",
                "bad path with spaces",
                "",
                std::iter::empty(),
                Bytes::new(),
                None,
                &mut sink,
            )
            .await
        });
        assert!(
            result.is_err(),
            "streaming dispatch must reject malformed path"
        );
    }

    #[test]
    fn malformed_path_split_returns_error_not_panic() {
        let result = block_on(async {
            dispatch_and_split(
                crate::Router::new(),
                "GET",
                "bad path with spaces",
                "",
                None,
                std::iter::empty(),
                None,
                Body::empty(),
                false,
            )
            .await
        });
        assert!(
            result.is_err(),
            "dispatch_and_split must reject malformed path"
        );
    }
}
