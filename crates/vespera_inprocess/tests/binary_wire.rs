//! Round-trip integration tests for the binary wire format
//! ([`vespera_inprocess::dispatch_from_bytes`]).
//!
//! Each test builds a wire-format request, dispatches it through a
//! dedicated test router (registered exactly once via `register_app`
//! by the FIRST test in this binary to run), and asserts on the
//! decoded wire response.  The wire format is:
//!
//! ```text
//! [u32 BE header_len][UTF-8 JSON header][raw body bytes]
//! ```

use std::collections::HashMap;
use std::sync::Once;

use axum::Router;
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Mutex;
use tokio::runtime::Builder;
use vespera_inprocess::{RequestChunk, dispatch_from_bytes, register_app};

// ── Test app ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PingQuery {
    msg: Option<String>,
}

async fn ping() -> &'static str {
    "pong"
}

async fn echo_text(body: String) -> String {
    body
}

async fn echo_bytes(headers: HeaderMap, body: Bytes) -> Response {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    ([(header::CONTENT_TYPE, ct)], body).into_response()
}

async fn query(Query(q): Query<PingQuery>) -> String {
    q.msg.unwrap_or_default()
}

async fn multi_header() -> Response {
    use axum::http::HeaderName;
    let mut headers = HeaderMap::new();
    let name = HeaderName::from_static("set-cookie");
    headers.append(name.clone(), "a=1".parse().unwrap());
    headers.append(name, "b=2".parse().unwrap());
    (headers, "ok").into_response()
}

fn test_router() -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/echo/text", post(echo_text))
        .route("/echo/bytes", post(echo_bytes))
        .route("/query", get(query))
        .route("/multi", get(multi_header))
}

fn install_router() {
    static INIT: Once = Once::new();
    INIT.call_once(|| register_app(test_router));
}

// ── Wire helpers ─────────────────────────────────────────────────────

fn encode_wire(
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: HashMap<&str, &str>,
    body: &[u8],
) -> Vec<u8> {
    let headers_json: serde_json::Map<String, Value> = headers
        .into_iter()
        .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
        .collect();
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String(method.to_owned()));
    header.insert("path".to_owned(), Value::String(path.to_owned()));
    if let Some(q) = query {
        header.insert("query".to_owned(), Value::String(q.to_owned()));
    }
    if !headers_json.is_empty() {
        header.insert("headers".to_owned(), Value::Object(headers_json));
    }
    let header_bytes = serde_json::to_vec(&Value::Object(header)).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len() + body.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire.extend_from_slice(body);
    wire
}

fn decode_wire(resp: &[u8]) -> (Value, Vec<u8>) {
    assert!(resp.len() >= 4, "wire response too short ({})", resp.len());
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(
        4 + header_len <= resp.len(),
        "header_len {header_len} overflows response ({} bytes)",
        resp.len()
    );
    let header: Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("response header JSON parses");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

fn dispatch(wire: Vec<u8>) -> (Value, Vec<u8>) {
    install_router();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let resp = dispatch_from_bytes(wire, &runtime);
    decode_wire(&resp)
}

// ── Tests ────────────────────────────────────────────────────────────

#[test]
fn get_text_response_roundtrip() {
    let (header, body) = dispatch(encode_wire("GET", "/ping", None, HashMap::new(), &[]));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(String::from_utf8_lossy(&body), "pong");
}

#[test]
fn post_json_body_echoes_back() {
    let json = br#"{"foo":"bar"}"#;
    let (header, body) = dispatch(encode_wire(
        "POST",
        "/echo/text",
        None,
        HashMap::from([("content-type", "application/json")]),
        json,
    ));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, json);
}

#[test]
fn post_octet_stream_preserves_non_utf8_bytes() {
    // Includes 0x00, 0xFF, and an invalid UTF-8 sequence (0xC0 0xC0).
    let raw: Vec<u8> = vec![
        0x00, 0x01, 0x02, 0xC0, 0xC0, 0xFE, 0xFF, 0xDE, 0xAD, 0xBE, 0xEF,
    ];
    let (header, body) = dispatch(encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &raw,
    ));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, raw, "binary body must round-trip byte-for-byte");
}

#[test]
fn query_string_is_forwarded() {
    let (header, body) = dispatch(encode_wire(
        "GET",
        "/query",
        Some("msg=hello%20world"),
        HashMap::new(),
        &[],
    ));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(String::from_utf8_lossy(&body), "hello world");
}

#[test]
fn multiple_set_cookie_headers_collapse_to_multi() {
    let (header, _body) = dispatch(encode_wire("GET", "/multi", None, HashMap::new(), &[]));
    assert_eq!(header["status"].as_u64(), Some(200));
    let set_cookie = &header["headers"]["set-cookie"];
    let arr = set_cookie
        .as_array()
        .unwrap_or_else(|| panic!("expected array for repeated set-cookie, got {set_cookie:#}"));
    let values: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        values.contains(&"a=1") && values.contains(&"b=2"),
        "expected both cookies preserved, got {values:?}"
    );
}

#[test]
fn unknown_path_returns_404() {
    let (header, _body) = dispatch(encode_wire(
        "GET",
        "/does-not-exist",
        None,
        HashMap::new(),
        &[],
    ));
    assert_eq!(header["status"].as_u64(), Some(404));
}

#[test]
fn invalid_http_method_returns_405() {
    let (header, body) = dispatch(encode_wire(
        "GET WITH SPACES",
        "/ping",
        None,
        HashMap::new(),
        &[],
    ));
    assert_eq!(header["status"].as_u64(), Some(405));
    assert!(
        String::from_utf8_lossy(&body).contains("Method Not Allowed"),
        "body should explain method invalidity"
    );
}

#[test]
fn large_body_one_mib_roundtrips() {
    let big: Vec<u8> = (0..1024u32 * 1024)
        .map(|i| u8::try_from(i % 256).expect("mod 256 fits in u8"))
        .collect();
    let (header, body) = dispatch(encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &big,
    ));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body.len(), big.len(), "size match");
    assert_eq!(body, big, "1 MiB body must round-trip byte-for-byte");
}

#[test]
fn empty_body_request_returns_text_response() {
    let (header, body) = dispatch(encode_wire("GET", "/ping", None, HashMap::new(), &[]));
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_from_bytes_async_inside_runtime() {
    // The async API runs natively in a Tokio runtime without
    // block_on — required by callers like the JNI dispatchAsync path.
    install_router();
    let wire = encode_wire("GET", "/ping", None, HashMap::new(), &[]);
    let resp = vespera_inprocess::dispatch_from_bytes_async(wire).await;
    let (header, body) = decode_wire(&resp);
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(body, b"pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_streaming_async_chunks_text_body() {
    install_router();
    let wire = encode_wire("GET", "/ping", None, HashMap::new(), &[]);
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let header_bytes = vespera_inprocess::dispatch_streaming_async(wire, |chunk| {
        chunks.push(chunk.to_vec());
    })
    .await;
    let (header, body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(200));
    assert!(
        body.is_empty(),
        "streaming response wire must carry no body (it goes via on_chunk)"
    );
    let collected: Vec<u8> = chunks.into_iter().flatten().collect();
    assert_eq!(collected, b"pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_streaming_async_large_binary_body() {
    install_router();
    let big_payload: Vec<u8> = (0u32..256 * 1024)
        .map(|i| u8::try_from(i % 256).expect("mod 256"))
        .collect();
    let wire = encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &big_payload,
    );
    let mut received: Vec<u8> = Vec::with_capacity(big_payload.len());
    let header_bytes = vespera_inprocess::dispatch_streaming_async(wire, |chunk| {
        received.extend_from_slice(chunk);
    })
    .await;
    let (header, _body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(
        received, big_payload,
        "256 KiB binary body must round-trip byte-for-byte via streaming callback"
    );
}

#[test]
fn wire_response_bytes_are_deterministic_across_dispatches() {
    // Response headers serialise from a BTreeMap — identical requests
    // MUST produce byte-identical wire responses (golden-file /
    // SHA-comparison safety).  This pins the V2-C determinism
    // guarantee; with the previous HashMap the JSON key order varied
    // per response.
    install_router();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    // /echo/bytes responds with content-type + content-length —
    // multiple headers, which is what exposed the ordering issue.
    let wire = encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        b"determinism-probe",
    );
    let first = dispatch_from_bytes(wire.clone(), &runtime);
    for run in 0..4 {
        let again = dispatch_from_bytes(wire.clone(), &runtime);
        assert_eq!(
            first, again,
            "wire response bytes must be identical on repeat dispatch (run {run})"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_bidirectional_streaming_roundtrips_small_body() {
    install_router();

    // Wire request header (no body — body comes via pull_chunk).
    let header_only_wire = encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    // Request body chunks to push.
    let chunks: Vec<Vec<u8>> = vec![b"hello ".to_vec(), b"world".to_vec(), b"!".to_vec()];
    let chunks_iter = Mutex::new(chunks.into_iter());
    let pull_chunk = move || -> RequestChunk {
        chunks_iter
            .lock()
            .unwrap()
            .next()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    // Response body sink.
    let received: std::sync::Arc<Mutex<Vec<u8>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
    let received_clone = std::sync::Arc::clone(&received);
    let on_chunk = move |chunk: &[u8]| {
        received_clone.lock().unwrap().extend_from_slice(chunk);
    };

    let header_bytes =
        vespera_inprocess::dispatch_bidirectional_streaming(header_only_wire, pull_chunk, on_chunk)
            .await;

    let (header, body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(200));
    assert!(body.is_empty(), "header-only response must carry no body");

    let final_body = received.lock().unwrap().clone();
    assert_eq!(
        String::from_utf8_lossy(&final_body),
        "hello world!",
        "request body chunks must roundtrip through the handler verbatim"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_bidirectional_streaming_endless_empty_pull_aborts_not_hangs() {
    install_router();

    let header_only_wire = encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    // A hostile producer that ALWAYS reports an empty chunk (mirrors a
    // non-conformant InputStream.read() returning 0 forever).  Without
    // the consecutive-empty cap this busy-spins the blocking-pool thread
    // forever; with it, the producer aborts the body so the dispatch
    // terminates.  A timeout guards against regression to a hang.
    let pull_chunk = || -> RequestChunk { RequestChunk::Data(Vec::new()) };
    let on_chunk = |_: &[u8]| {};

    let dispatched = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        vespera_inprocess::dispatch_bidirectional_streaming(header_only_wire, pull_chunk, on_chunk),
    )
    .await;

    let header_bytes = dispatched.expect("dispatch must terminate, not busy-spin forever");
    let (header, _body) = decode_wire(&header_bytes);
    assert_eq!(
        header["status"].as_u64(),
        Some(400),
        "endless empty reads must abort the upload (400), not hang"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_bidirectional_streaming_pull_error_aborts_upload() {
    install_router();

    let header_only_wire = encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    // First pull yields a chunk, the second reports a producer error
    // (e.g. the source `InputStream` threw mid-upload).  The body must
    // abort so the handler's `Bytes` extractor fails — NOT be accepted
    // as a clean EOF carrying the partial "hello ".
    let counter = Mutex::new(0u32);
    let pull_chunk = move || -> RequestChunk {
        let mut g = counter.lock().unwrap();
        *g += 1;
        match *g {
            1 => RequestChunk::Data(b"hello ".to_vec()),
            _ => RequestChunk::Error,
        }
    };

    let received: std::sync::Arc<Mutex<Vec<u8>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
    let received_clone = std::sync::Arc::clone(&received);
    let on_chunk = move |chunk: &[u8]| {
        received_clone.lock().unwrap().extend_from_slice(chunk);
    };

    let header_bytes =
        vespera_inprocess::dispatch_bidirectional_streaming(header_only_wire, pull_chunk, on_chunk)
            .await;

    let (header, _body) = decode_wire(&header_bytes);
    // axum's `Bytes` extractor rejects a body that errors mid-stream
    // (400), instead of the 200 echo of the partial "hello " that the
    // old silent-EOF behaviour would have produced.
    assert_eq!(
        header["status"].as_u64(),
        Some(400),
        "a producer error must reject the upload, not silently complete it"
    );
    // Whatever streams back is axum's 400 rejection body — never the
    // partial "hello " echoed as a successful upload.
    let echoed = received.lock().unwrap().clone();
    assert_ne!(
        echoed.as_slice(),
        b"hello ",
        "the aborted upload must not be echoed back as a completed body"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_bidirectional_streaming_empty_chunk_is_retry_not_eof() {
    // Pins the pull contract relied on by the JNI bridge:
    // `Some(vec![])` means "no data right now, keep pulling" (mirrors
    // Java `InputStream.read(byte[]) == 0`), NOT end-of-stream.  Data
    // arriving AFTER an empty chunk must still reach the handler.
    install_router();

    let header_only_wire = encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let chunks: Vec<Vec<u8>> = vec![
        b"before".to_vec(),
        Vec::new(), // empty read — must be skipped, not treated as EOF
        b" after".to_vec(),
    ];
    let chunks_iter = Mutex::new(chunks.into_iter());
    let pull_chunk = move || -> RequestChunk {
        chunks_iter
            .lock()
            .unwrap()
            .next()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let received: std::sync::Arc<Mutex<Vec<u8>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
    let received_clone = std::sync::Arc::clone(&received);
    let on_chunk = move |chunk: &[u8]| {
        received_clone.lock().unwrap().extend_from_slice(chunk);
    };

    let header_bytes =
        vespera_inprocess::dispatch_bidirectional_streaming(header_only_wire, pull_chunk, on_chunk)
            .await;

    let (header, _body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(200));
    assert_eq!(
        String::from_utf8_lossy(&received.lock().unwrap()),
        "before after",
        "data after an empty pull chunk must still reach the handler"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatch_bidirectional_streaming_large_request_body() {
    install_router();

    let header_only_wire = encode_wire(
        "POST",
        "/echo/bytes",
        None,
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    // 512 KiB request body split into ~16 KiB chunks — the size where
    // the bounded mpsc channel (16 slots) will exercise backpressure.
    let total_size = 512 * 1024;
    let chunk_size = 16 * 1024;
    let n_chunks = total_size / chunk_size;
    let request_chunks: Vec<Vec<u8>> = (0..n_chunks)
        .map(|i| {
            (0..chunk_size)
                .map(|j| u8::try_from((i * chunk_size + j) % 256).expect("mod 256"))
                .collect()
        })
        .collect();
    let expected: Vec<u8> = request_chunks.iter().flatten().copied().collect();
    let chunks_iter = Mutex::new(request_chunks.into_iter());
    let pull_chunk = move || -> RequestChunk {
        chunks_iter
            .lock()
            .unwrap()
            .next()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let received: std::sync::Arc<Mutex<Vec<u8>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
    let received_clone = std::sync::Arc::clone(&received);
    let on_chunk = move |chunk: &[u8]| {
        received_clone.lock().unwrap().extend_from_slice(chunk);
    };

    let header_bytes =
        vespera_inprocess::dispatch_bidirectional_streaming(header_only_wire, pull_chunk, on_chunk)
            .await;

    let (header, _) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(200));

    let final_body = received.lock().unwrap().clone();
    assert_eq!(final_body.len(), expected.len(), "size match");
    assert_eq!(
        final_body, expected,
        "512 KiB request body must round-trip byte-for-byte"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_bidirectional_streaming_emits_error_wire_on_malformed_header() {
    install_router();
    let bad_header: Vec<u8> = vec![0u8, 0, 0, 99]; // overflow
    let pull = || -> RequestChunk { RequestChunk::End };
    let on = |_: &[u8]| {};

    let header_bytes =
        vespera_inprocess::dispatch_bidirectional_streaming(bad_header, pull, on).await;
    let (header, body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(400));
    assert!(!body.is_empty(), "error response carries explanatory body");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_streaming_async_emits_error_wire_on_malformed_input() {
    install_router();
    let bad_wire: Vec<u8> = vec![0u8, 0, 0, 99]; // header_len overflow
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let header_bytes = vespera_inprocess::dispatch_streaming_async(bad_wire, |chunk| {
        chunks.push(chunk.to_vec());
    })
    .await;
    // On error the streaming variant emits a normal error_wire — header + body
    // both inside the returned bytes (no callback invocation).
    let (header, body) = decode_wire(&header_bytes);
    assert_eq!(header["status"].as_u64(), Some(400));
    assert!(
        !body.is_empty(),
        "error response must carry the error message in its body"
    );
    assert!(
        chunks.is_empty(),
        "no chunks should fire on malformed input"
    );
}
