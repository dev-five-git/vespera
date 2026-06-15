//! Integration tests for the `*_with_header` streaming variants
//! ([`dispatch_streaming_with_header_async`],
//! [`dispatch_bidirectional_streaming_with_header`]) and the other
//! streaming error paths that the regular `binary_wire.rs` suite
//! doesn't exercise.
//!
//! These variants are what Spring `HttpServletResponse`-style hosts
//! call: `on_header` fires before the response is committed, then
//! `on_chunk` streams the body.  Both callbacks must be invoked
//! exactly once on every code path (success or error).

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::task::{Context, Poll};

use axum::Router;
use axum::http::HeaderMap;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use serde_json::Value;
use vespera_inprocess::{
    DirectWriteResult, RequestChunk, dispatch_bidirectional_streaming_closing,
    dispatch_bidirectional_streaming_with_header,
    dispatch_bidirectional_streaming_with_header_closing, dispatch_into_async,
    dispatch_streaming_async, dispatch_streaming_with_header_async, register_app_named,
};

// ── Test app ─────────────────────────────────────────────────────────

async fn ping() -> &'static str {
    "pong"
}

async fn echo_bytes(headers: HeaderMap, body: Bytes) -> Response {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    ([(header::CONTENT_TYPE, ct)], body).into_response()
}

async fn triple_header() -> Response {
    use axum::http::{HeaderName, HeaderValue};
    let mut headers = HeaderMap::new();
    let name = HeaderName::from_static("x-trace-id");
    headers.append(name.clone(), HeaderValue::from_static("a"));
    headers.append(name.clone(), HeaderValue::from_static("b"));
    headers.append(name, HeaderValue::from_static("c"));
    (headers, "trace").into_response()
}

/// Echoes back the request query string, used to confirm that
/// `dispatch_and_split` reassembles `path?query` correctly.
async fn echo_query(uri: axum::http::Uri) -> String {
    uri.query().unwrap_or("").to_owned()
}

/// Handler that returns immediately WITHOUT consuming the request
/// body — used to exercise the producer's "receiver dropped" break
/// branch in `bidirectional_streaming_inner` when chunks are pushed
/// faster than the body is consumed.
async fn discard_body() -> &'static str {
    "ok"
}

/// Panics before producing any status/headers — exercises the
/// "handler panic before the header callback fires" path that the JNI
/// layer's `header_sent` fallback depends on.
async fn panic_before_header() -> Response {
    panic!("intentional handler panic for test");
}

/// Reads the full request body — which lazily starts the bidirectional
/// producer — and THEN panics, so the panic unwinds past the explicit
/// request-source close. Used to verify the RAII close guard still fires
/// `request_close` on a panic unwind (the panic-path sibling of M3).
async fn read_then_panic(_body: Bytes) -> Response {
    panic!("intentional panic after reading request body");
}

/// Response body that yields one data frame and then errors — simulates a
/// handler streaming from a source (file / DB / upstream) that fails
/// mid-stream. Used to verify a body error is never reported as a clean
/// (truncated) success.
struct ErroringBody {
    sent_first: bool,
}

impl HttpBody for ErroringBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.sent_first {
            Poll::Ready(Some(Err("simulated mid-stream body failure".into())))
        } else {
            self.sent_first = true;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"partial")))))
        }
    }
}

async fn erroring_body_handler() -> Response {
    Response::new(axum::body::Body::new(ErroringBody { sent_first: false }))
}

struct MultiChunkBody {
    index: usize,
}

impl HttpBody for MultiChunkBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let chunk = [
            b"first".as_slice(),
            b"second".as_slice(),
            b"third".as_slice(),
        ]
        .get(self.index)
        .copied();
        self.index += 1;
        Poll::Ready(chunk.map(|bytes| Ok(Frame::data(Bytes::copy_from_slice(bytes)))))
    }
}

async fn multi_chunk_body() -> Response {
    Response::new(axum::body::Body::new(MultiChunkBody { index: 0 }))
}

fn make_router() -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/echo", post(echo_bytes))
        .route("/triple", get(triple_header))
        .route("/q", get(echo_query))
        .route("/discard", post(discard_body))
        .route("/panic", get(panic_before_header))
        .route("/read-panic", post(read_then_panic))
        .route("/err-body", get(erroring_body_handler))
        .route("/multi-chunk", get(multi_chunk_body))
}

fn install_router() {
    static INIT: Once = Once::new();
    INIT.call_once(|| register_app_named("with_header_test", make_router));
}

// ── Wire helpers ─────────────────────────────────────────────────────

fn encode_wire(method: &str, path: &str, headers: HashMap<&str, &str>, body: &[u8]) -> Vec<u8> {
    let headers_json: serde_json::Map<String, Value> = headers
        .into_iter()
        .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
        .collect();
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String(method.to_owned()));
    header.insert("path".to_owned(), Value::String(path.to_owned()));
    header.insert(
        "app".to_owned(),
        Value::String("with_header_test".to_owned()),
    );
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

fn encode_bad_version(method: &str, path: &str) -> Vec<u8> {
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(99u8)); // wrong version
    header.insert("method".to_owned(), Value::String(method.to_owned()));
    header.insert("path".to_owned(), Value::String(path.to_owned()));
    header.insert(
        "app".to_owned(),
        Value::String("with_header_test".to_owned()),
    );
    let header_bytes = serde_json::to_vec(&Value::Object(header)).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire
}

fn encode_unknown_app(method: &str, path: &str) -> Vec<u8> {
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String(method.to_owned()));
    header.insert("path".to_owned(), Value::String(path.to_owned()));
    header.insert(
        "app".to_owned(),
        Value::String("definitely-no-such-app".to_owned()),
    );
    let header_bytes = serde_json::to_vec(&Value::Object(header)).expect("header serialise");
    let header_len = u32::try_from(header_bytes.len()).expect("header fits in u32");
    let mut wire = Vec::with_capacity(4 + header_bytes.len());
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(&header_bytes);
    wire
}

fn decode_wire(resp: &[u8]) -> (Value, Vec<u8>) {
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    let header: Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("header JSON parses");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

// ── dispatch_streaming_with_header_async ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_with_header_emits_header_before_chunks() {
    install_router();
    let wire = encode_wire("GET", "/ping", HashMap::new(), &[]);
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let c = Arc::clone(&chunks);

    dispatch_streaming_with_header_async(
        wire,
        move |bytes| h.lock().unwrap().extend_from_slice(bytes),
        move |chunk| {
            c.lock().unwrap().push(chunk.to_vec());
            ControlFlow::Continue(())
        },
    )
    .await;

    let header_bytes = header_buf.lock().unwrap().clone();
    let (header_json, body_in_header) = decode_wire(&header_bytes);
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert!(
        body_in_header.is_empty(),
        "header-only wire must not include body"
    );
    let body: Vec<u8> = chunks.lock().unwrap().iter().flatten().copied().collect();
    assert_eq!(body, b"pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_with_header_error_on_short_input_skips_chunk_callback() {
    let bad_wire: Vec<u8> = vec![0u8, 0]; // less than 4 bytes
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let c = Arc::clone(&chunks);

    dispatch_streaming_with_header_async(
        bad_wire,
        move |bytes| h.lock().unwrap().extend_from_slice(bytes),
        move |chunk| {
            c.lock().unwrap().push(chunk.to_vec());
            ControlFlow::Continue(())
        },
    )
    .await;

    let header_bytes = header_buf.lock().unwrap().clone();
    let (header_json, body) = decode_wire(&header_bytes);
    assert_eq!(header_json["status"].as_u64(), Some(400));
    assert!(!body.is_empty(), "error body must explain the failure");
    assert!(
        chunks.lock().unwrap().is_empty(),
        "on_chunk must not fire on error path"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_with_header_error_on_version_mismatch() {
    install_router();
    let bad = encode_bad_version("GET", "/ping");
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let c = Arc::clone(&chunks);

    dispatch_streaming_with_header_async(
        bad,
        move |bytes| h.lock().unwrap().extend_from_slice(bytes),
        move |chunk| {
            c.lock().unwrap().push(chunk.to_vec());
            ControlFlow::Continue(())
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(400));
    assert!(chunks.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_with_header_error_on_unknown_app() {
    install_router();
    let bad = encode_unknown_app("GET", "/ping");
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let c = Arc::clone(&chunks);

    dispatch_streaming_with_header_async(
        bad,
        move |bytes| h.lock().unwrap().extend_from_slice(bytes),
        move |chunk| {
            c.lock().unwrap().push(chunk.to_vec());
            ControlFlow::Continue(())
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(404));
    assert!(chunks.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_with_header_invalid_method_returns_405_via_header_callback() {
    install_router();
    let wire = encode_wire("BAD METHOD", "/ping", HashMap::new(), &[]);
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let c = Arc::clone(&chunks);

    dispatch_streaming_with_header_async(
        wire,
        move |bytes| h.lock().unwrap().extend_from_slice(bytes),
        move |chunk| {
            c.lock().unwrap().push(chunk.to_vec());
            ControlFlow::Continue(())
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(405));
    assert!(chunks.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_with_header_forwards_query_string_via_dispatch_and_split() {
    install_router();
    // Hand-roll a wire that includes a `query` field to exercise the
    // `format!("{path}?{query}")` reassembly inside dispatch_and_split.
    let mut header = serde_json::Map::new();
    header.insert("v".to_owned(), Value::from(1u8));
    header.insert("method".to_owned(), Value::String("GET".into()));
    header.insert("path".to_owned(), Value::String("/q".into()));
    header.insert("query".to_owned(), Value::String("hello=world".into()));
    header.insert("app".to_owned(), Value::String("with_header_test".into()));
    let hb = serde_json::to_vec(&Value::Object(header)).unwrap();
    let hlen = u32::try_from(hb.len()).unwrap();
    let mut wire = Vec::with_capacity(4 + hb.len());
    wire.extend_from_slice(&hlen.to_be_bytes());
    wire.extend_from_slice(&hb);

    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let c = Arc::clone(&chunks);

    dispatch_streaming_with_header_async(
        wire,
        move |bytes| h.lock().unwrap().extend_from_slice(bytes),
        move |chunk| {
            c.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(
        String::from_utf8(chunks.lock().unwrap().clone()).unwrap(),
        "hello=world"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_with_header_triple_header_collapses_into_multi() {
    install_router();
    let wire = encode_wire("GET", "/triple", HashMap::new(), &[]);
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let chunks: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let c = Arc::clone(&chunks);

    dispatch_streaming_with_header_async(
        wire,
        move |bytes| h.lock().unwrap().extend_from_slice(bytes),
        move |chunk| {
            c.lock().unwrap().push(chunk.to_vec());
            ControlFlow::Continue(())
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    let trace = &header_json["headers"]["x-trace-id"];
    let arr = trace
        .as_array()
        .unwrap_or_else(|| panic!("expected multi-header array, got {trace:#}"));
    let values: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(
        values,
        vec!["a", "b", "c"],
        "3rd append must extend the Multi vec, got {values:?}"
    );
}

// ── dispatch_bidirectional_streaming_with_header ─────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_roundtrips_body() {
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let chunks = vec![b"foo".to_vec(), b"bar".to_vec()];
    let chunks_iter = Mutex::new(chunks.into_iter());
    let pull = move || -> RequestChunk {
        chunks_iter
            .lock()
            .unwrap()
            .next()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"foobar");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_error_on_short_input() {
    let bad: Vec<u8> = vec![0u8, 0, 0]; // < 4 bytes
    let pull = || -> RequestChunk { RequestChunk::End };
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        bad,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, body) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(400));
    assert!(!body.is_empty());
    assert!(body_buf.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_error_on_version_mismatch() {
    install_router();
    let bad = encode_bad_version("POST", "/echo");
    let pull = || -> RequestChunk { RequestChunk::End };
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        bad,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(400));
    assert!(body_buf.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_error_on_unknown_app() {
    install_router();
    let bad = encode_unknown_app("POST", "/echo");
    let pull = || -> RequestChunk { RequestChunk::End };
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        bad,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(404));
    assert!(body_buf.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_invalid_method_returns_405() {
    install_router();
    let wire = encode_wire("BAD METHOD", "/echo", HashMap::new(), &[]);
    let pull = || -> RequestChunk { RequestChunk::End };
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(405));
    assert!(body_buf.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_break_when_receiver_dropped_mid_stream() {
    // Producer pushes many non-empty chunks; the handler ignores the
    // body and returns immediately, so the bounded 16-slot mpsc fills
    // up, `tx.blocking_send` blocks, and once the request body is
    // dropped (handler finished) the send fails and the producer
    // takes the `break` branch.  Pull counter must end short of the
    // 1000-chunk source — proving the early break ran.
    install_router();
    let wire = encode_wire(
        "POST",
        "/discard",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = Arc::clone(&counter);
    let pull = move || -> RequestChunk {
        let mut g = counter_clone.lock().unwrap();
        if *g >= 1000 {
            return RequestChunk::End;
        }
        *g += 1;
        // 4 KiB chunks — large enough that 16 slots ≈ 64 KiB worth
        // pile up before the handler decides to return.
        RequestChunk::Data(vec![0u8; 4096])
    };

    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    let pulled = *counter.lock().unwrap();
    assert!(
        pulled < 1000,
        "producer should have aborted early (got {pulled} of 1000 pulls before break)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_slow_producer_yields_poll_pending() {
    // Producer sleeps between chunks so the consumer polls an empty
    // channel and exercises the `Poll::Pending` arm of ChannelBody.
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = Arc::clone(&counter);
    let pull = move || -> RequestChunk {
        let mut g = counter_clone.lock().unwrap();
        if *g >= 3 {
            return RequestChunk::End;
        }
        *g += 1;
        // Sleep so the consumer drains the channel and hits Pending.
        std::thread::sleep(std::time::Duration::from_millis(25));
        RequestChunk::Data(b"chunk".to_vec())
    };

    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"chunkchunkchunk");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_empty_pull_chunks_are_skipped() {
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    // First call returns Some(empty) — must be skipped, not treated as EOF.
    // Second call returns the real body, third returns None (EOF).
    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = Arc::clone(&counter);
    let pull = move || -> RequestChunk {
        let mut g = counter_clone.lock().unwrap();
        *g += 1;
        match *g {
            1 => RequestChunk::Data(Vec::new()), // empty chunk — must be skipped
            2 => RequestChunk::Data(b"X".to_vec()),
            _ => RequestChunk::End,
        }
    };

    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);

    dispatch_bidirectional_streaming_with_header(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"X");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_with_header_handler_panic_does_not_emit_header() {
    // Precondition lock for the JNI layer's `header_sent` fallback: when
    // an axum handler panics BEFORE producing status/headers, the panic
    // propagates through dispatch_streaming_with_header_async (the
    // inprocess layer does NOT catch it) and `on_header` is never called.
    // The JNI symbol relies on exactly this — its catch_unwind sees the
    // panic with `header_sent == false` and emits a 500 header itself.
    install_router();
    let wire = encode_wire("GET", "/panic", HashMap::new(), &[]);

    let header_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hs = Arc::clone(&header_seen);

    // Drive it on a spawned task so the handler panic surfaces as a
    // JoinError instead of unwinding the test thread.
    let join = tokio::spawn(async move {
        dispatch_streaming_with_header_async(
            wire,
            move |_header: &[u8]| {
                hs.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            |_chunk: &[u8]| ControlFlow::Continue(()),
        )
        .await;
    })
    .await;

    assert!(
        join.is_err(),
        "a handler panic must propagate (inprocess does not catch it)"
    );
    assert!(
        !header_seen.load(std::sync::atomic::Ordering::SeqCst),
        "on_header must NOT fire when the handler panics before producing a header"
    );
}

// ── M3: request-source close hook ────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_closing_invokes_close_after_full_read() {
    // M3 regression: when the handler reads the request body (which
    // lazily starts the producer), the `request_close` hook fires
    // exactly once after the response is drained. This is what lets the
    // JNI layer close a Java `InputStream` so a producer parked in a
    // blocking read can't hang the dispatch on a stuck upload.
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let chunks = vec![b"foo".to_vec(), b"bar".to_vec()];
    let chunks_iter = Mutex::new(chunks.into_iter());
    let pull = move || -> RequestChunk {
        chunks_iter
            .lock()
            .unwrap()
            .next()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&body_buf);
    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    let header = dispatch_bidirectional_streaming_closing(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move || {
            cc.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header);
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"foobar");
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        1,
        "request_close must fire exactly once after a full-read dispatch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_closing_invokes_close_after_full_read() {
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let payload = Mutex::new(Some(b"payload".to_vec()));
    let pull = move || -> RequestChunk {
        payload
            .lock()
            .unwrap()
            .take()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);
    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    dispatch_bidirectional_streaming_with_header_closing(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
        move || {
            cc.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"payload");
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        1,
        "request_close must fire exactly once after a full-read dispatch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_closing_skips_close_when_body_ignored() {
    // When the handler never reads the request body, the producer is
    // never started, so there is nothing to close — `request_close` must
    // NOT fire. A GET handler with no body extractor never polls the
    // request body.
    install_router();
    let wire = encode_wire("GET", "/ping", HashMap::new(), &[]);

    let pull = || -> RequestChunk { RequestChunk::End };
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);
    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    dispatch_bidirectional_streaming_with_header_closing(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
        move || {
            cc.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        0,
        "request_close must NOT fire when the handler ignores the body (producer never started)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_closing_invokes_close_on_handler_panic() {
    // Panic-path sibling of M3: the handler reads the full body (starting the
    // producer) and then panics, so the unwind skips the explicit close. The
    // RAII guard in bidirectional_streaming_inner must STILL fire request_close
    // so a producer parked in a blocking source read can be unblocked instead
    // of leaking forever.
    install_router();
    let wire = encode_wire(
        "POST",
        "/read-panic",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let payload = Mutex::new(Some(b"body".to_vec()));
    let pull = move || -> RequestChunk {
        payload
            .lock()
            .unwrap()
            .take()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    // Run on a spawned task so the handler panic surfaces as a JoinError
    // instead of unwinding the test thread.
    let join = tokio::spawn(async move {
        dispatch_bidirectional_streaming_with_header_closing(
            wire,
            pull,
            |_chunk: &[u8]| ControlFlow::Continue(()),
            |_hdr: &[u8]| {},
            move || {
                cc.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;
    })
    .await;

    assert!(
        join.is_err(),
        "handler panic must propagate (inprocess does not catch it)"
    );
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        1,
        "request_close must fire via the drop guard even when the handler panics after starting the producer"
    );
}

// ── Response body stream errors must not be reported as success ───────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn response_streaming_body_error_yields_500_not_truncated_success() {
    // A handler whose response body errors mid-stream must surface a 500
    // through the returned wire header, not the original 200 with a silently
    // truncated body (dispatch_response_streaming path).
    install_router();
    let wire = encode_wire("GET", "/err-body", HashMap::new(), &[]);
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&body_buf);

    let header = dispatch_streaming_async(wire, move |chunk| {
        b.lock().unwrap().extend_from_slice(chunk);
        ControlFlow::Continue(())
    })
    .await;

    let (header_json, err_body) = decode_wire(&header);
    assert_eq!(
        header_json["status"].as_u64(),
        Some(500),
        "a response body that errors mid-stream must yield 500, not a truncated 200"
    );
    assert!(
        !err_body.is_empty(),
        "the 500 wire must carry an error body"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn response_streaming_stops_draining_when_chunk_callback_breaks() {
    install_router();
    let wire = encode_wire("GET", "/multi-chunk", HashMap::new(), &[]);
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&body_buf);

    let header = dispatch_streaming_async(wire, move |chunk| {
        b.lock().unwrap().extend_from_slice(chunk);
        ControlFlow::Break(())
    })
    .await;

    let (header_json, header_body) = decode_wire(&header);
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert!(header_body.is_empty());
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"first");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_write_body_error_yields_500_not_truncated_success() {
    // Direct-write path: the response is buffered into the caller's slice and
    // only returned at the end, so a body error must rewrite the buffer to a
    // 500 error wire rather than returning the partially-written 200 bytes.
    install_router();
    let wire = encode_wire("GET", "/err-body", HashMap::new(), &[]);
    let mut out = vec![0u8; 4096];

    let result = dispatch_into_async(wire, &mut out).await;
    let n = match result {
        DirectWriteResult::Complete(n) => n,
        DirectWriteResult::Overflow(required) => {
            panic!("expected Complete (500 fits in 4096), got Overflow({required})")
        }
    };

    let (header_json, _) = decode_wire(&out[..n]);
    assert_eq!(
        header_json["status"].as_u64(),
        Some(500),
        "direct-write must emit 500 on a body error, not truncated bytes"
    );
}
