use super::{
    ChannelBody, RequestProducerHandle, RequestSourceCloser, StreamOutcome,
    emit_header_then_stream_body,
};
use crate::envelope::ResponseMetadata;
use axum::body::Body;
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// A panicking user close hook must be CONTAINED by `close_if_started`:
/// the method also runs from `Drop` during unwind, where an escaping panic
/// would be a double-panic → process `abort()`.  Build a "started" producer
/// handle (a real `JoinHandle`, so `producer_was_started` is true and the
/// hook actually runs), then assert the call returns normally despite the
/// hook panicking, and that a second call is a consumed-hook no-op.
///
/// Without the `catch_unwind` in `close_if_started`, the first call would
/// unwind out of this `#[test]` (and, on a real `Drop`-during-unwind path,
/// abort the process).
#[test]
fn close_hook_panic_is_contained() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime");
    // `Runtime::spawn` hands back a live `JoinHandle` without entering the
    // runtime (the empty task is never driven or awaited) — we only need a
    // handle present so the producer counts as "started".
    let join_handle = runtime.spawn(async {});
    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(Some(join_handle)));

    let mut closer = RequestSourceCloser::new(Arc::clone(&producer_handle), || panic!("hook boom"));
    // Returns normally — the panic is caught inside `close_if_started`.
    closer.close_if_started();
    // Idempotent: the hook was consumed on the first call, so this is a
    // no-op and does not panic a second time.
    closer.close_if_started();
}

struct ErrorBody;

impl HttpBody for ErrorBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(Some(Err(std::io::Error::other("validation body failed"))))
    }
}

fn decode_status(wire: &[u8]) -> u64 {
    let header_len = u32::from_be_bytes(wire[..4].try_into().expect("wire prefix")) as usize;
    let header: serde_json::Value =
        serde_json::from_slice(&wire[4..4 + header_len]).expect("response header JSON");
    header["status"].as_u64().expect("numeric response status")
}

#[tokio::test]
async fn validation_body_error_commits_a_complete_500_header() {
    let mut header = Vec::new();
    let mut chunks = Vec::new();
    let outcome = emit_header_then_stream_body(
        422,
        http::HeaderMap::new(),
        ResponseMetadata::current(),
        Body::new(ErrorBody),
        &mut |bytes| header.extend_from_slice(bytes),
        &mut |bytes| {
            chunks.extend_from_slice(bytes);
            ControlFlow::Continue(())
        },
    )
    .await;

    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(decode_status(&header), 500);
    assert!(chunks.is_empty());
}

#[tokio::test]
async fn validation_body_sink_stop_is_reported_after_422_header() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    let mut header = Vec::new();
    let outcome = emit_header_then_stream_body(
        422,
        headers,
        ResponseMetadata::current(),
        Body::from(br#"{"errors":[{"path":"name"}]}"#.as_slice()),
        &mut |bytes| header.extend_from_slice(bytes),
        &mut |_bytes| ControlFlow::Break(()),
    )
    .await;

    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert_eq!(decode_status(&header), 422);
}

#[tokio::test]
async fn channel_body_without_a_producer_is_clean_eof() {
    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(None));
    let mut body = ChannelBody {
        rx: None,
        pull_chunk: None,
        capacity: 1,
        producer_handle,
    };

    body.start_producer_if_needed();
    assert!(body.frame().await.is_none());
}
