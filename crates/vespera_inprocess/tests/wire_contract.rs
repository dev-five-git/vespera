//! **Wire-format contract locks** — byte-exact goldens for the
//! response wire header.
//!
//! These tests pin the serialized JSON *bytes* (field order, header
//! key order, `HeaderValue` untagged shape, metadata layout) so any
//! refactor of `collect_header_map` / wire serialization that changes
//! the observable wire format fails loudly.  Do NOT update the
//! expected strings without an explicit wire-format review — Java
//! decoders and HMAC-style byte comparisons depend on this layout.

use std::collections::HashMap;
use std::sync::Once;

use axum::Router;
use axum::http::{HeaderMap, HeaderName};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::Value;
use tokio::runtime::Builder;
use vespera_inprocess::{dispatch_from_bytes, error_wire, register_app};

async fn contract_headers() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-single"),
        "value-1".parse().unwrap(),
    );
    let cookie = HeaderName::from_static("set-cookie");
    headers.append(cookie.clone(), "a=1".parse().unwrap());
    headers.append(cookie, "b=2".parse().unwrap());
    (headers, "ok").into_response()
}

/// Echo the raw request body back — used by the cross-language golden
/// test so a matched `POST /users` proves the header/body split + routing
/// on the exact bytes the Java encoder produces.
async fn echo_body(body: axum::body::Bytes) -> axum::body::Bytes {
    body
}

fn install_router() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        register_app(|| {
            Router::new()
                .route("/contract", get(contract_headers))
                .route("/users", post(echo_body))
        });
    });
}

fn encode_wire(method: &str, path: &str, headers: HashMap<&str, &str>, body: &[u8]) -> Vec<u8> {
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String(method.to_owned()));
    header.insert("path".to_owned(), Value::String(path.to_owned()));
    if !headers.is_empty() {
        let headers_json: serde_json::Map<String, Value> = headers
            .into_iter()
            .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
            .collect();
        header.insert("headers".to_owned(), Value::Object(headers_json));
    }
    let header_bytes = serde_json::to_vec(&Value::Object(header)).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

fn split_wire(resp: &[u8]) -> (String, Vec<u8>) {
    assert!(resp.len() >= 4, "wire response too short");
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(
        4 + header_len <= resp.len(),
        "header_len overflows response"
    );
    let header = String::from_utf8(resp[4..4 + header_len].to_vec()).expect("UTF-8 header");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

/// Golden: response wire header bytes for a multi-value-header
/// response.  Locks:
/// - struct field order: `v`, `status`, `headers`, `metadata`
/// - BTreeMap alphabetical header key order
/// - `HeaderValue` untagged shape (string vs array)
/// - compact JSON (no whitespace)
#[test]
fn response_wire_header_bytes_are_locked() {
    install_router();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let resp = dispatch_from_bytes(
        encode_wire("GET", "/contract", HashMap::new(), &[]),
        &runtime,
    );
    let (header, body) = split_wire(&resp);
    assert_eq!(body, b"ok");

    // wire-order locked — see module docs before changing.
    let expected = format!(
        concat!(
            r#"{{"v":1,"status":200,"headers":{{"#,
            r#""content-length":"2","#,
            r#""content-type":"text/plain; charset=utf-8","#,
            r#""set-cookie":["a=1","b=2"],"#,
            r#""x-single":"value-1""#,
            r#"}},"metadata":{{"version":"{version}"}}}}"#
        ),
        version = env!("CARGO_PKG_VERSION"),
    );
    assert_eq!(
        header, expected,
        "wire response header bytes drifted — this is a WIRE FORMAT BREAK"
    );
}

/// Golden: `error_wire` bytes.  Locks the error path's exact shape —
/// content-type single value + plain-text body.
#[test]
fn error_wire_bytes_are_locked() {
    let wire = error_wire(418, "teapot says no");
    let (header, body) = split_wire(&wire);
    assert_eq!(body, b"teapot says no");

    // wire-order locked — see module docs before changing.
    let expected = format!(
        concat!(
            r#"{{"v":1,"status":418,"headers":{{"#,
            r#""content-type":"text/plain; charset=utf-8""#,
            r#"}},"metadata":{{"version":"{version}"}}}}"#
        ),
        version = env!("CARGO_PKG_VERSION"),
    );
    assert_eq!(
        header, expected,
        "error_wire header bytes drifted — this is a WIRE FORMAT BREAK"
    );
}

/// **Cross-language golden (request direction)** — dispatches the
/// byte-identical wire frame the Java encoder produces and asserts the
/// Rust parser accepts it and routes correctly.
///
/// The header JSON + body below are byte-identical to the Java side's
/// shared golden (`VesperaWireTest.CANONICAL_REQUEST_HEADER_JSON` /
/// `CANONICAL_REQUEST_BODY`).  Java asserts its encoder emits exactly
/// these bytes; this test asserts Rust parses exactly these bytes and
/// routes `POST /users` with the body intact.  Together they lock the two
/// independent hand-rolled wire implementations against silent drift: a
/// change to either side's field order / structure / framing breaks its
/// own golden assertion.
#[test]
fn cross_language_request_golden_routes() {
    install_router();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // Byte-identical to the Java cross-language golden — do NOT edit one
    // side without the other (see VesperaWireTest).
    let header_json =
        br#"{"v":1,"method":"POST","path":"/users","query":"page=1","headers":{"content-type":"application/json"}}"#;
    let body = br#"{"x":1}"#;
    let mut wire = Vec::with_capacity(4 + header_json.len() + body.len());
    wire.extend_from_slice(&u32::try_from(header_json.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(header_json);
    wire.extend_from_slice(body);

    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, resp_body) = split_wire(&resp);

    // Body round-trip proves the parser split header/body at the exact
    // offset and routed the echo handler; 200 proves `POST /users` matched.
    assert_eq!(
        resp_body, body,
        "cross-language request golden: echo body must round-trip (header/body split + routing)"
    );
    assert!(
        header.contains(r#""status":200"#),
        "cross-language request golden: POST /users must route 200 — got: {header}"
    );
}

/// Golden: 422 hoisting shape — `validation_errors` appears as the
/// LAST field, after `metadata`, with `path`/`message` entry order.
#[test]
fn validation_hoist_wire_bytes_are_locked() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        vespera_inprocess::register_app_named("contract-422", || {
            Router::new().route(
                "/reject",
                get(|| async {
                    (
                        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                        [("content-type", "application/json")],
                        r#"{"errors":[{"path":"email","message":"not a valid email"}]}"#,
                    )
                }),
            )
        });
    });
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut req_header = serde_json::Map::new();
    req_header.insert("v".to_owned(), Value::from(1u8));
    req_header.insert("method".to_owned(), Value::String("GET".to_owned()));
    req_header.insert("path".to_owned(), Value::String("/reject".to_owned()));
    req_header.insert("app".to_owned(), Value::String("contract-422".to_owned()));
    let header_bytes = serde_json::to_vec(&Value::Object(req_header)).expect("serialise");
    let mut wire = Vec::with_capacity(4 + header_bytes.len());
    wire.extend_from_slice(&u32::try_from(header_bytes.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(&header_bytes);

    let resp = dispatch_from_bytes(wire, &runtime);
    let (header, body) = split_wire(&resp);
    assert_eq!(
        body, br#"{"errors":[{"path":"email","message":"not a valid email"}]}"#,
        "original 422 body must be preserved verbatim"
    );

    // wire-order locked — see module docs before changing.
    let expected = format!(
        concat!(
            r#"{{"v":1,"status":422,"headers":{{"#,
            r#""content-length":"59","#,
            r#""content-type":"application/json""#,
            r#"}},"metadata":{{"version":"{version}"}},"#,
            r#""validation_errors":[{{"path":"email","message":"not a valid email"}}]}}"#
        ),
        version = env!("CARGO_PKG_VERSION"),
    );
    assert_eq!(
        header, expected,
        "422 hoisting wire bytes drifted — this is a WIRE FORMAT BREAK"
    );
}
