//! Internal dispatch plumbing shared by every public entry point:
//! request building, router oneshot driving, and response collection.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
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
    let request =
        build_request_from_bytes(method_str, path, query, headers, body_bytes, header_bytes_owner)?;

    let response = match router.oneshot(request).await {
        Ok(response) => response,
        // axum routers are `Service<_, Error = Infallible>`; the `Err`
        // variant is uninhabited, so this match is exhaustive and emits
        // no panic/unwind site on this FFI-adjacent hot path.
        Err(err) => match err {},
    };

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

/// Build the axum request shared by the buffered ([`dispatch_parts`]) and
/// response-streaming ([`dispatch_response_streaming`]) paths — both take a
/// fully-buffered [`Bytes`] body and default a missing `Content-Type`.
///
/// One borrowed-iterator pass applies every header while detecting
/// `Content-Type` (case-insensitive, RFC 7230 §3.2); a non-empty body with
/// no `Content-Type` defaults to `application/json`.  Returns `Err((405, _))`
/// for an unparseable method and `Err((400, _))` for a malformed path / header,
/// upholding the "every failure returns a wire response" contract.
///
/// Constructs the [`Request`] **directly** — `Request::new(body)` then
/// in-place method / URI / header assignment — instead of threading the
/// `http::request::Builder` state machine, which re-checks an internal
/// `Result<Parts>` and is moved by value on every `.method`/`.uri`/`.header`
/// call.  The `HeaderMap` is pre-reserved from the header count so insertion
/// never triggers an incremental grow; a bodyless, headerless request
/// reserves `0` and never allocates a bucket (preserving the DIRECT-`GET`
/// zero-allocation sweet spot).  Header names/values are parsed with the same
/// `HeaderName::from_bytes` / `HeaderValue::from_str` the builder used and are
/// `append`ed (not `insert`ed), so the built request is byte-identical
/// including duplicate-name multi-value semantics.  `#[inline]` so the two
/// call sites keep inlined codegen.
#[inline]
fn build_request_from_bytes<'h>(
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body_bytes: Bytes,
    header_bytes_owner: Option<&Bytes>,
) -> Result<Request<Body>, (u16, String)> {
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };
    let uri = build_uri(path, query)?;
    let body_is_empty = body_bytes.is_empty();

    let mut request = Request::new(Body::from(body_bytes));
    *request.method_mut() = http_method;
    *request.uri_mut() = uri;

    // Reserve exactly what we append: the wire headers plus, for a non-empty
    // body, the possible default content-type.  A bodyless, headerless
    // request reserves 0 and never allocates a HeaderMap bucket.
    let reserve = headers
        .size_hint()
        .0
        .saturating_add(usize::from(!body_is_empty));
    let header_map = request.headers_mut();
    if reserve > 0 {
        header_map.reserve(reserve);
    }

    // Case-insensitive Content-Type detection (RFC 7230 §3.2), tracked
    // inside the single header pass.
    let mut has_content_type = false;
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| (400, format!("invalid request: {e}")))?;
        let header_value = header_value_from_owner(value, header_bytes_owner)
            .map_err(|e| (400, format!("invalid request: {e}")))?;
        // `HeaderName::from_bytes` already ASCII-lowercased the name, so the
        // `== CONTENT_TYPE` standard-header comparison replaces the raw
        // `eq_ignore_ascii_case` byte-fold scan with a (typically) cheap
        // standard-header discriminant compare.  Behaviour is identical: a
        // name that case-insensitively equals "content-type" is always a
        // valid token that `from_bytes` normalises to `CONTENT_TYPE`, and the
        // comparison still happens before `append` consumes `header_name`.
        has_content_type = has_content_type || header_name == CONTENT_TYPE;
        header_map.append(header_name, header_value);
    }
    if !body_is_empty && !has_content_type {
        header_map.append(
            CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
    }
    Ok(request)
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
    let request =
        build_request_from_bytes(method_str, path, query, headers, body_bytes, header_bytes_owner)?;

    let response = match router.oneshot(request).await {
        Ok(response) => response,
        // axum routers are `Service<_, Error = Infallible>`; the `Err`
        // variant is uninhabited, so this match is exhaustive and emits
        // no panic/unwind site on this FFI-adjacent hot path.
        Err(err) => match err {},
    };

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
        match resp_headers.entry(name.as_str().to_owned()) {
            Entry::Vacant(e) => {
                e.insert(HeaderValue::Single(val_str));
            }
            Entry::Occupied(mut e) => {
                let slot = e.get_mut();
                let new_slot = match std::mem::replace(slot, HeaderValue::Single(String::new())) {
                    HeaderValue::Single(prev) => HeaderValue::Multi(vec![prev, val_str]),
                    HeaderValue::Multi(mut v) => {
                        v.push(val_str);
                        HeaderValue::Multi(v)
                    }
                };
                *slot = new_slot;
            }
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
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };
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

    // Direct construction — see [`build_request_from_bytes`]: bypass the
    // `http::request::Builder` state machine and pre-reserve the HeaderMap so
    // header insertion never triggers an incremental grow.  Headers are
    // `append`ed (multi-value preserving); the body is opaque here, so
    // content-type defaulting follows the caller's `default_json_content_type`
    // flag rather than body-emptiness detection.
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
    // Detect Content-Type during the single insertion pass (RFC 7230 §3.2
    // case-insensitive) instead of a separate caller-side pre-scan.
    let mut has_content_type = false;
    for (name, value) in headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| (400, format!("invalid request: {e}")))?;
        let header_value = header_value_from_owner(value, header_bytes_owner)
            .map_err(|e| (400, format!("invalid request: {e}")))?;
        // `HeaderName::from_bytes` already ASCII-lowercased the name, so the
        // `== CONTENT_TYPE` standard-header comparison replaces the raw
        // `eq_ignore_ascii_case` byte-fold scan with a (typically) cheap
        // standard-header discriminant compare.  Behaviour is identical: a
        // name that case-insensitively equals "content-type" is always a
        // valid token that `from_bytes` normalises to `CONTENT_TYPE`, and the
        // comparison still happens before `append` consumes `header_name`.
        has_content_type = has_content_type || header_name == CONTENT_TYPE;
        header_map.append(header_name, header_value);
    }
    if default_json_when_absent && !has_content_type {
        header_map.append(
            CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
    }

    let response = match router.oneshot(request).await {
        Ok(response) => response,
        // axum routers are `Service<_, Error = Infallible>`; the `Err`
        // variant is uninhabited, so this match is exhaustive and emits
        // no panic/unwind site on this FFI-adjacent hot path.
        Err(err) => match err {},
    };

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
