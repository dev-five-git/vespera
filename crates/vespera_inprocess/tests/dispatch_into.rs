//! Integration tests for the direct-write dispatch API
//! ([`vespera_inprocess::dispatch_into_async`]) — the
//! zero-materialisation path used by the JNI direct-buffer symbol.

use std::sync::Once;

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post};
use bytes::Bytes;
use serde_json::{Value, json};
use tokio::runtime::Builder;
use vespera_inprocess::{
    DirectWriteResult, dispatch_from_bytes, dispatch_into, dispatch_into_async_borrowed,
    register_app,
};

async fn ping() -> &'static str {
    "pong"
}

async fn echo(body: Bytes) -> Bytes {
    body
}

/// Mimics the `Validated<T>` 422 contract: JSON body with an `errors`
/// array — the wire layer must hoist it into the response header.
async fn reject() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"errors": [{"path": "name", "message": "too short"}]})),
    )
}

fn install() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        register_app(|| {
            Router::new()
                .route("/ping", get(ping))
                .route("/echo", post(echo))
                .route("/reject", post(reject))
        });
    });
}

fn encode(method: &str, path: &str, body: &[u8]) -> Vec<u8> {
    let header = json!({
        "v": 1,
        "method": method,
        "path": path,
        "headers": {"content-type": "application/octet-stream"},
    });
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&u32::try_from(header_bytes.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

fn decode(wire: &[u8]) -> (Value, Vec<u8>) {
    let header_len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&wire[4..4 + header_len]).unwrap();
    (header, wire[4 + header_len..].to_vec())
}

fn runtime() -> tokio::runtime::Runtime {
    Builder::new_current_thread().enable_all().build().unwrap()
}

#[test]
fn complete_matches_dispatch_from_bytes_exactly() {
    install();
    let rt = runtime();
    let body = vec![0xCDu8; 32 * 1024];
    let wire = encode("POST", "/echo", &body);

    let reference = dispatch_from_bytes(wire.clone(), &rt);
    let mut out = vec![0u8; reference.len() + 64];
    let result = dispatch_into(wire, &mut out, &rt);

    // V2-C determinism makes byte-equality a valid assertion.
    assert_eq!(result, DirectWriteResult::Complete(reference.len()));
    assert_eq!(&out[..reference.len()], &reference[..]);
}

#[test]
fn exact_fit_boundary() {
    install();
    let rt = runtime();
    let wire = encode("GET", "/ping", &[]);
    let reference = dispatch_from_bytes(wire.clone(), &rt);

    let mut out = vec![0u8; reference.len()];
    let result = dispatch_into(wire, &mut out, &rt);
    assert_eq!(result, DirectWriteResult::Complete(reference.len()));
    assert_eq!(out, reference);
}

#[test]
fn overflow_reports_exact_required_size() {
    install();
    let rt = runtime();
    let body = vec![0xABu8; 100 * 1024];
    let wire = encode("POST", "/echo", &body);
    let reference_len = dispatch_from_bytes(wire.clone(), &rt).len();

    // Out buffer big enough for the header but not the body.
    let mut out = vec![0u8; 256];
    let result = dispatch_into(wire.clone(), &mut out, &rt);
    assert_eq!(result, DirectWriteResult::Overflow(reference_len));

    // Header smaller than even the wire header → still exact.
    let mut tiny = vec![0u8; 4];
    let result = dispatch_into(wire, &mut tiny, &rt);
    assert_eq!(result, DirectWriteResult::Overflow(reference_len));
}

#[test]
fn status_422_preserves_validation_error_hoisting() {
    install();
    let rt = runtime();
    let wire = encode("POST", "/reject", b"{}");

    let reference = dispatch_from_bytes(wire.clone(), &rt);
    let (ref_header, _) = decode(&reference);
    assert!(
        ref_header["validation_errors"].is_array(),
        "precondition: byte path hoists validation_errors"
    );

    let mut out = vec![0u8; reference.len() + 64];
    let DirectWriteResult::Complete(n) = dispatch_into(wire, &mut out, &rt) else {
        panic!("422 must fit");
    };
    assert_eq!(
        &out[..n],
        &reference[..],
        "422 direct path must be byte-identical to dispatch_from_bytes \
         (hoisting + body verbatim)"
    );
    let (header, body) = decode(&out[..n]);
    assert_eq!(header["status"].as_u64(), Some(422));
    assert!(
        header["validation_errors"].is_array(),
        "hoisted validation_errors present"
    );
    assert!(!body.is_empty(), "original 422 body preserved verbatim");
}

#[test]
fn pre_dispatch_errors_write_error_wire_into_out() {
    install();
    let rt = runtime();

    // Unknown app → 404 wire response written into out.
    let header = json!({"v": 1, "method": "GET", "path": "/ping", "app": "ghost"});
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let mut wire = u32::try_from(header_bytes.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    wire.extend_from_slice(&header_bytes);

    let mut out = vec![0u8; 4096];
    let DirectWriteResult::Complete(n) = dispatch_into(wire, &mut out, &rt) else {
        panic!("error wire must fit in 4096 bytes");
    };
    let (resp_header, body) = decode(&out[..n]);
    assert_eq!(resp_header["status"].as_u64(), Some(404));
    assert!(String::from_utf8_lossy(&body).contains("ghost"));

    // Bad wire version → 400.
    let bad = encode("GET", "/ping", &[]);
    let mut bad = bad;
    // Patch "v":1 → "v":9 inside the JSON header.
    let pos = bad
        .windows(4)
        .position(|w| w == b"\"v\":")
        .expect("v field present");
    bad[pos + 4] = b'9';
    let DirectWriteResult::Complete(n) = dispatch_into(bad, &mut out, &rt) else {
        panic!("400 wire must fit");
    };
    let (resp_header, _) = decode(&out[..n]);
    assert_eq!(resp_header["status"].as_u64(), Some(400));
}

#[test]
fn overflow_then_retry_with_exact_size_succeeds() {
    install();
    let rt = runtime();
    let body = vec![0x42u8; 8 * 1024];
    let wire = encode("POST", "/echo", &body);

    let mut small = vec![0u8; 16];
    let DirectWriteResult::Overflow(required) = dispatch_into(wire.clone(), &mut small, &rt) else {
        panic!("expected overflow");
    };

    let mut exact = vec![0u8; required];
    let result = dispatch_into(wire.clone(), &mut exact, &rt);
    assert_eq!(result, DirectWriteResult::Complete(required));
    assert_eq!(exact, dispatch_from_bytes(wire, &rt));
}

#[test]
fn body_without_content_type_matches_byte_path() {
    // Regression for the Content-Type defaulting drift: dispatch_parts
    // injects `content-type: application/json` for non-empty bodies
    // without one; the direct-write path must do the same or JSON
    // extractors behave differently across dispatch modes.
    install();
    let rt = runtime();
    let header = json!({"v": 1, "method": "POST", "path": "/echo"}); // no headers at all
    let header_bytes = serde_json::to_vec(&header).unwrap();
    let body = b"{\"k\":1}";
    let mut wire = u32::try_from(header_bytes.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);

    let reference = dispatch_from_bytes(wire.clone(), &rt);
    let mut out = vec![0u8; reference.len() + 64];
    let result = dispatch_into(wire, &mut out, &rt);
    assert_eq!(result, DirectWriteResult::Complete(reference.len()));
    assert_eq!(
        &out[..reference.len()],
        &reference[..],
        "direct path must apply the same content-type defaulting as the byte path"
    );
}

#[test]
fn borrowed_matches_byte_path_bodyless_with_body_and_422() {
    // The borrowed direct-write path (the JNI dispatchDirect0 entry) must be
    // byte-identical to the owned byte path across: a bodyless GET (zero input
    // copy), a POST with a body (body-only copy), and a 422 (validation_errors
    // hoisting through the shared finish_direct_write tail).
    install();
    let rt = runtime();
    for (method, path, body) in [
        ("GET", "/ping", Vec::new()),
        ("POST", "/echo", vec![0x5Au8; 4096]),
        ("POST", "/reject", b"{}".to_vec()),
    ] {
        let wire = encode(method, path, &body);
        let reference = dispatch_from_bytes(wire.clone(), &rt);
        let mut out = vec![0u8; reference.len() + 64];
        let result = rt.block_on(dispatch_into_async_borrowed(&wire, &mut out));
        assert_eq!(
            result,
            DirectWriteResult::Complete(reference.len()),
            "{method} {path}: borrowed must complete with the byte-path length"
        );
        assert_eq!(
            &out[..reference.len()],
            &reference[..],
            "{method} {path}: borrowed direct-write must be byte-identical to the byte path"
        );
    }
}
