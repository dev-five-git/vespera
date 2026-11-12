//! Header-inspection routes for the JNI E2E suite: which request headers
//! reach Rust (the app-selection header must not), and the raw-byte
//! (Latin-1 contract) round trip of non-ASCII header values.

use std::net::SocketAddr;

use vespera::axum::{
    Extension, Json,
    extract::ConnectInfo,
    http::{HeaderMap, HeaderValue, header, uri::Scheme},
    response::{IntoResponse, Response},
};

/// Names of the request headers the handler received, sorted.
#[allow(clippy::unused_async)]
#[vespera::route(get, tags = ["headers"])]
pub async fn names(headers: HeaderMap) -> Json<Vec<String>> {
    let mut names: Vec<String> = headers.keys().map(ToString::to_string).collect();
    names.sort();
    Json(names)
}

/// Echo the raw bytes of the `x-name` request header as the body and as a
/// `Content-Disposition` filename.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/raw", tags = ["headers"])]
pub async fn raw(headers: HeaderMap) -> Response {
    let name = headers
        .get("x-name")
        .map(|value| value.as_bytes().to_vec())
        .unwrap_or_default();
    let mut disposition = b"attachment; filename=\"".to_vec();
    disposition.extend_from_slice(&name);
    disposition.push(b'"');
    // `x-name` already passed header-value validation, and quoting it keeps it valid.
    let disposition =
        HeaderValue::from_bytes(&disposition).unwrap_or(HeaderValue::from_static("attachment"));
    ([(header::CONTENT_DISPOSITION, disposition)], name).into_response()
}

/// The client address and scheme the Java proxy forwarded on the wire
/// (`remote_addr` / `scheme`), as `"<scheme> <ip:port>"`.
#[allow(clippy::unused_async)]
#[vespera::route(get, path = "/client", tags = ["headers"])]
pub async fn client(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    scheme: Option<Extension<Scheme>>,
) -> String {
    let scheme = scheme.map_or_else(|| "none".to_owned(), |Extension(s)| s.to_string());
    format!("{scheme} {addr}")
}
