//! Internal dispatch plumbing shared by every public entry point:
//! request building, router oneshot driving, and response collection.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ops::ControlFlow;

use axum::body::Body;
use bytes::Bytes;
use http::{Method, Request};
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

/// Drive a [`Router`] with the supplied envelope fields and return
/// raw response parts.
///
/// Returns `Err((status, msg))` only for pre-dispatch errors
/// (currently only "invalid HTTP method" → 405).  Router/handler
/// errors cannot occur because axum routers are
/// `Service<_, Error = Infallible>`.
pub async fn dispatch_parts<'h>(
    router: Router,
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body_bytes: Bytes,
) -> Result<ResponseParts, (u16, String)> {
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };

    let mut builder = request_builder(http_method, path, query);
    // Case-insensitive Content-Type detection (RFC 7230 §3.2),
    // tracked inside the single header pass.
    let mut has_content_type = false;
    for (name, value) in headers {
        has_content_type = has_content_type || name.eq_ignore_ascii_case("content-type");
        builder = builder.header(name, value);
    }
    if !body_bytes.is_empty() && !has_content_type {
        builder = builder.header("content-type", "application/json");
    }

    // A malformed wire `path` (e.g. a raw space → not a valid
    // `http::Uri`) or an invalid header name/value surfaces here as a
    // builder error; convert it to a 400 so the contract "every failure
    // returns a wire response" holds instead of panicking.
    let request = match builder.body(Body::from(body_bytes)) {
        Ok(req) => req,
        Err(e) => return Err((400, format!("invalid request: {e}"))),
    };

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

    Ok(collect_response_parts(response).await)
}

/// Start a request builder with method + URI.  When `query` is empty
/// the borrowed `path` feeds `Uri` parsing directly — no intermediate
/// `String`; otherwise a single exact-capacity join is allocated.
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
pub async fn dispatch_response_streaming<'h, F>(
    router: Router,
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body_bytes: Bytes,
    on_chunk: &mut F,
) -> Result<(u16, http::HeaderMap, ResponseMetadata), (u16, String)>
where
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
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

    // A malformed wire `path` (e.g. a raw space → not a valid
    // `http::Uri`) or an invalid header name/value surfaces here as a
    // builder error; convert it to a 400 so the contract "every failure
    // returns a wire response" holds instead of panicking.
    let request = match builder.body(Body::from(body_bytes)) {
        Ok(req) => req,
        Err(e) => return Err((400, format!("invalid request: {e}"))),
    };

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

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
                    break;
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
async fn collect_response_parts(response: axum::response::Response) -> ResponseParts {
    let (parts, body) = response.into_parts();

    let body_bytes = body
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();

    (
        parts.status.as_u16(),
        parts.headers,
        body_bytes,
        ResponseMetadata::current(),
    )
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
/// `default_json_content_type` adds `content-type: application/json`
/// to the outgoing request (mirroring [`dispatch_parts`]'s defaulting)
/// — only [`dispatch_into_async`] sets it, because streaming callers
/// hand this function an opaque [`Body`] whose emptiness is
/// unknowable up front.
pub async fn dispatch_and_split<'h>(
    router: Router,
    method_str: &str,
    path: &str,
    query: &str,
    headers: impl Iterator<Item = (&'h str, &'h str)>,
    body: Body,
    default_json_content_type: bool,
) -> Result<(u16, http::HeaderMap, ResponseMetadata, Body), (u16, String)> {
    let Ok(http_method) = method_str.parse::<Method>() else {
        return Err((
            405,
            format!("Method Not Allowed: '{method_str}' is not a valid HTTP method"),
        ));
    };

    let mut builder = request_builder(http_method, path, query);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    if default_json_content_type {
        builder = builder.header("content-type", "application/json");
    }

    // Same contract as dispatch_parts: a malformed path/header must
    // surface as a 400 wire response, not a panic.
    let request = match builder.body(body) {
        Ok(req) => req,
        Err(e) => return Err((400, format!("invalid request: {e}"))),
    };

    let response = router
        .oneshot(request)
        .await
        .expect("router error is Infallible");

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
                std::iter::empty(),
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
