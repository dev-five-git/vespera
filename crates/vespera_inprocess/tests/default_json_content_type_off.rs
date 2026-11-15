//! `set_default_json_content_type(false)`: a non-empty request body without
//! a `Content-Type` reaches the handler **without** one on every dispatch
//! path (buffered, direct, response-streaming, bidirectional, text
//! envelope), so axum's `Json<T>` rejects it with `415` instead of accepting
//! a cross-site "simple" request.  An explicit `Content-Type` is still
//! forwarded.  Own process: the knob is a process-wide first-wins setting
//! (the default-on behaviour is covered by `misc_coverage.rs`).

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Once;

use axum::Router;
use axum::http::{HeaderMap, header};
use axum::routing::post;
use serde_json::Value;
use vespera_inprocess::{
    DirectWriteResult, RequestChunk, RequestEnvelope, default_json_content_type,
    dispatch_bidirectional_streaming, dispatch_from_bytes, dispatch_into, dispatch_owned,
    dispatch_streaming_async, register_app, set_default_json_content_type,
};

async fn echo_ct(headers: HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("MISSING")
        .to_owned()
}

async fn json_echo(axum::Json(value): axum::Json<Value>) -> String {
    value.to_string()
}

fn app() -> Router {
    Router::new()
        .route("/echo_ct", post(echo_ct))
        .route("/json", post(json_echo))
}

fn setup() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        assert!(set_default_json_content_type(false));
        register_app(app);
    });
    assert!(!default_json_content_type());
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn wire(path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let headers: HashMap<&str, &str> = headers.iter().copied().collect();
    let header = serde_json::to_vec(
        &serde_json::json!({"v": 1, "method": "POST", "path": path, "headers": headers}),
    )
    .unwrap();
    let mut out = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&header);
    out.extend_from_slice(body);
    out
}

fn decode(resp: &[u8]) -> (u64, String) {
    let len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&resp[4..4 + len]).unwrap();
    (
        header["status"].as_u64().unwrap(),
        String::from_utf8_lossy(&resp[4 + len..]).into_owned(),
    )
}

#[test]
fn buffered_path_does_not_default() {
    setup();
    let runtime = rt();
    let resp = dispatch_from_bytes(wire("/echo_ct", &[], b"{}"), &runtime);
    assert_eq!(decode(&resp), (200, "MISSING".to_owned()));
    // `Json<T>` now refuses an untyped body …
    let resp = dispatch_from_bytes(wire("/json", &[], b"{\"a\":1}"), &runtime);
    assert_eq!(decode(&resp).0, 415);
    // … and still accepts an explicitly typed one.
    let typed = [("content-type", "application/json")];
    let resp = dispatch_from_bytes(wire("/json", &typed, b"{\"a\":1}"), &runtime);
    assert_eq!(decode(&resp), (200, "{\"a\":1}".to_owned()));
    let plain = [("content-type", "text/plain")];
    let resp = dispatch_from_bytes(wire("/echo_ct", &plain, b"hi"), &runtime);
    assert_eq!(decode(&resp), (200, "text/plain".to_owned()));
}

#[test]
fn direct_path_does_not_default() {
    setup();
    let runtime = rt();
    let mut out = vec![0u8; 4096];
    let DirectWriteResult::Complete(n) =
        dispatch_into(wire("/echo_ct", &[], b"{}"), &mut out, &runtime)
    else {
        panic!("4 KiB fits the response");
    };
    assert_eq!(decode(&out[..n]), (200, "MISSING".to_owned()));
}

#[test]
fn response_streaming_path_does_not_default() {
    setup();
    let mut body = Vec::new();
    let header = rt().block_on(dispatch_streaming_async(
        wire("/echo_ct", &[], b"{}"),
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    ));
    assert_eq!(decode(&header).0, 200);
    assert_eq!(body, b"MISSING");
}

#[test]
fn bidirectional_path_does_not_default() {
    setup();
    let mut chunks = vec![RequestChunk::Data(b"{}".to_vec()), RequestChunk::End].into_iter();
    let mut body = Vec::new();
    let header = rt().block_on(dispatch_bidirectional_streaming(
        wire("/echo_ct", &[], b""),
        move || chunks.next().unwrap_or(RequestChunk::End),
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    ));
    assert_eq!(decode(&header).0, 200);
    assert_eq!(body, b"MISSING");
}

#[test]
fn text_envelope_does_not_default() {
    setup();
    let envelope = RequestEnvelope {
        method: "POST".into(),
        path: "/echo_ct".into(),
        body: "{}".into(),
        ..Default::default()
    };
    let resp = rt().block_on(dispatch_owned(app(), envelope));
    assert_eq!((resp.status, resp.body.as_str()), (200, "MISSING"));
}
