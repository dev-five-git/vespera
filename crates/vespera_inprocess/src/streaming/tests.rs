use super::{
    ChannelBody, RequestProducerHandle, RequestSourceCloser, StreamOutcome,
    emit_header_then_stream_body,
};
use crate::bounded::Deadline;
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

struct SparseFrameBody {
    index: u8,
}

impl HttpBody for SparseFrameBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let frame = match self.index {
            0 => Some(Frame::trailers(http::HeaderMap::new())),
            1 => Some(Frame::data(Bytes::new())),
            2 => Some(Frame::data(Bytes::from_static(b"payload"))),
            _ => None,
        };
        self.index += 1;
        Poll::Ready(frame.map(Ok))
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
        &mut Deadline::disabled(),
        &mut |bytes| header.extend_from_slice(bytes),
        &mut |bytes| {
            chunks.extend_from_slice(bytes);
            ControlFlow::Continue(())
        },
    )
    .await;

    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(decode_status(&header), 500);
    assert_eq!(chunks, Vec::<u8>::new());
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
        &mut Deadline::disabled(),
        &mut |bytes| header.extend_from_slice(bytes),
        &mut |_bytes| ControlFlow::Break(()),
    )
    .await;

    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert_eq!(decode_status(&header), 422);
}

#[tokio::test]
async fn empty_validation_body_emits_only_a_complete_422_header() {
    let mut header = Vec::new();
    let mut chunk_calls = 0;
    let outcome = emit_header_then_stream_body(
        422,
        http::HeaderMap::new(),
        ResponseMetadata::current(),
        Body::empty(),
        &mut Deadline::disabled(),
        &mut |bytes| header.extend_from_slice(bytes),
        &mut |_bytes| {
            chunk_calls += 1;
            ControlFlow::Continue(())
        },
    )
    .await;

    let expected = crate::wire::build_wire_header_bytes_hoisting(
        422,
        &http::HeaderMap::new(),
        &ResponseMetadata::current(),
        &Bytes::new(),
    );
    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(header, expected);
    assert_eq!(chunk_calls, 0);
}

#[tokio::test]
async fn trailers_and_empty_data_frames_are_ignored_before_payload() {
    let mut header = Vec::new();
    let mut chunks = Vec::new();
    let outcome = emit_header_then_stream_body(
        200,
        http::HeaderMap::new(),
        ResponseMetadata::current(),
        Body::new(SparseFrameBody { index: 0 }),
        &mut Deadline::disabled(),
        &mut |bytes| header.extend_from_slice(bytes),
        &mut |bytes| {
            chunks.extend_from_slice(bytes);
            ControlFlow::Continue(())
        },
    )
    .await;

    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(decode_status(&header), 200);
    assert_eq!(chunks, b"payload");
}

#[tokio::test]
async fn channel_body_without_a_producer_is_clean_eof() {
    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(None));
    let mut body = ChannelBody {
        rx: None,
        pull_chunk: None,
        capacity: 1,
        producer_handle,
        max_bytes: 0,
        received: 0,
        limit_exceeded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        clean_end: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        permit_wait: None,
    };

    assert!(body.frame().await.is_none());
}

fn channel_body<P>(pull: P) -> ChannelBody
where
    P: FnMut() -> crate::RequestChunk + Send + 'static,
{
    ChannelBody::new(
        pull,
        Arc::new(Mutex::new(None)),
        0,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

/// A producer that reports a clean `End` yields its data then a clean EOF.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_body_clean_end_is_eof() {
    let mut sent = false;
    let mut body = channel_body(move || {
        if sent {
            crate::RequestChunk::End
        } else {
            sent = true;
            crate::RequestChunk::Data(b"abc".to_vec())
        }
    });
    let data = body.frame().await.expect("frame").expect("data");
    assert_eq!(data.into_data().expect("data frame"), "abc");
    assert!(
        body.frame().await.is_none(),
        "clean End must be a clean EOF"
    );
}

/// A channel that closes WITHOUT the producer having seen `End` — e.g. the
/// `spawn_blocking` producer was dropped unrun during runtime shutdown, so
/// its sender is dropped too — must surface as a body error, never as a
/// clean (silently truncated) EOF.
#[rstest::rstest]
#[case::dropped_without_end(false, true)]
#[case::clean_end(true, false)]
#[tokio::test]
async fn channel_close_is_eof_only_after_a_clean_end(
    #[case] clean_end: bool,
    #[case] expect_abort: bool,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(tx);
    let mut body = ChannelBody {
        rx: Some(rx),
        pull_chunk: None,
        capacity: 1,
        producer_handle: Arc::new(Mutex::new(None)),
        max_bytes: 0,
        received: 0,
        limit_exceeded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        clean_end: Arc::new(std::sync::atomic::AtomicBool::new(clean_end)),
        permit_wait: None,
    };
    let first = body.frame().await;
    assert_eq!(matches!(first, Some(Err(_))), expect_abort);
    assert_eq!(first.is_none(), !expect_abort);
    assert!(
        body.frame().await.is_none(),
        "terminal after the first close"
    );
}

/// The producer-slot semaphore admits uncontended producers without
/// boxing, and a saturated cap leaves the body pending until a slot frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producer_slot_waits_while_saturated() {
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let mut first = channel_body(|| crate::RequestChunk::End);
    let held = std::future::poll_fn(|cx| first.poll_producer_slot(&slots, cx)).await;
    assert!(held.is_some(), "uncontended slot");
    assert!(first.permit_wait.is_none(), "fast path does not box");

    let mut second = channel_body(|| crate::RequestChunk::End);
    let pending =
        std::future::poll_fn(|cx| Poll::Ready(second.poll_producer_slot(&slots, cx).is_pending()))
            .await;
    assert!(pending, "saturated cap waits");
    assert!(second.permit_wait.is_some());
    drop(held);
    let acquired = std::future::poll_fn(|cx| second.poll_producer_slot(&slots, cx)).await;
    assert!(acquired.is_some(), "freed slot is handed over");
    assert!(second.permit_wait.is_none());

    slots.close();
    let mut third = channel_body(|| crate::RequestChunk::End);
    let closed = std::future::poll_fn(|cx| third.poll_producer_slot(&slots, cx)).await;
    assert!(closed.is_none(), "a closed semaphore runs uncapped");
}
