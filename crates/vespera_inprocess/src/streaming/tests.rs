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
        false,
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
        false,
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
        false,
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
        false,
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
        slots: Arc::new(tokio::sync::Semaphore::new(1)),
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
        slots: Arc::new(tokio::sync::Semaphore::new(1)),
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
    first.slots = Arc::clone(&slots);
    let held = std::future::poll_fn(|cx| first.poll_producer_slot(cx)).await;
    assert!(held.is_some(), "uncontended slot");
    assert!(first.permit_wait.is_none(), "fast path does not box");

    let mut second = channel_body(|| crate::RequestChunk::End);
    second.slots = Arc::clone(&slots);
    let pending =
        std::future::poll_fn(|cx| Poll::Ready(second.poll_producer_slot(cx).is_pending())).await;
    assert!(pending, "saturated cap waits");
    assert!(second.permit_wait.is_some());
    drop(held);
    let acquired = std::future::poll_fn(|cx| second.poll_producer_slot(cx)).await;
    assert!(acquired.is_some(), "freed slot is handed over");
    assert!(second.permit_wait.is_none());

    slots.close();
    let mut third = channel_body(|| crate::RequestChunk::End);
    third.slots = Arc::clone(&slots);
    let closed = std::future::poll_fn(|cx| third.poll_producer_slot(cx)).await;
    assert!(closed.is_none(), "a closed semaphore runs uncapped");
}

/// A body whose producer cannot get a slot stays pending (no producer, no
/// thread) and starts once a slot frees up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_waits_for_a_producer_slot() {
    let slots = Arc::new(tokio::sync::Semaphore::new(0));
    let mut sent = false;
    let mut body = channel_body(move || {
        if std::mem::replace(&mut sent, true) {
            crate::RequestChunk::End
        } else {
            crate::RequestChunk::Data(b"x".to_vec())
        }
    });
    body.slots = Arc::clone(&slots);
    let mut pinned = std::pin::Pin::new(&mut body);
    let first =
        std::future::poll_fn(|cx| Poll::Ready(pinned.as_mut().poll_frame(cx).is_pending())).await;
    assert!(first, "no slot: the body waits");
    assert!(pinned.rx.is_none(), "producer not started");
    slots.add_permits(1);
    let frame = body.frame().await.expect("frame").expect("data");
    assert_eq!(frame.into_data().unwrap(), Bytes::from_static(b"x"));
    assert!(body.frame().await.is_none());
}

#[rstest::rstest]
#[case::unlimited(0, tokio::sync::Semaphore::MAX_PERMITS)]
#[case::capped(8, 8)]
#[case::clamped(usize::MAX, tokio::sync::Semaphore::MAX_PERMITS)]
fn producer_permits_map_the_cap(#[case] max: usize, #[case] permits: usize) {
    assert_eq!(super::producer::producer_permits(max), permits);
}

/// One scripted step of [`ScriptedBody`].
#[derive(Clone, Copy)]
enum Step {
    /// Not ready yet (wakes itself so the next poll proceeds).
    Wait,
    Data(&'static [u8]),
    Fail,
}

struct ScriptedBody {
    steps: Vec<Step>,
    next: usize,
}

impl HttpBody for ScriptedBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let Some(step) = self.steps.get(self.next).copied() else {
            return Poll::Ready(None);
        };
        self.next += 1;
        match step {
            Step::Wait => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Step::Data(bytes) => Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(bytes))))),
            Step::Fail => Poll::Ready(Some(Err(std::io::Error::other("body failed")))),
        }
    }
}

/// Runs the header-first tail over `steps`; the sink records every call
/// (`""` = flush hint) and answers `Break` on call number `break_at`.
async fn scripted(
    steps: Vec<Step>,
    flush_hints: bool,
    break_at: Option<usize>,
) -> (StreamOutcome, Vec<String>) {
    let mut calls = Vec::new();
    let mut header = Vec::new();
    let outcome = emit_header_then_stream_body(
        200,
        http::HeaderMap::new(),
        ResponseMetadata::current(),
        Body::new(ScriptedBody { steps, next: 0 }),
        &mut Deadline::disabled(),
        &mut |bytes| header.extend_from_slice(bytes),
        &mut |bytes| {
            calls.push(String::from_utf8_lossy(bytes).into_owned());
            if break_at == Some(calls.len()) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
        flush_hints,
    )
    .await;
    assert_eq!(decode_status(&header), 200);
    (outcome, calls)
}

#[rstest::rstest]
// Ready frames coalesce into ONE sink call; nothing waits, so no flush.
#[case::ready_frames_coalesce(
    vec![Step::Data(b"a"), Step::Data(b"b"), Step::Data(b"c")], true, None,
    StreamOutcome::Complete, &["abc"][..]
)]
// A waiting body delivers the batch then flushes (header-first paths).
#[case::flush_before_waiting(
    vec![Step::Wait, Step::Data(b"a"), Step::Data(b"b"), Step::Wait, Step::Data(b"c")], true, None,
    StreamOutcome::Complete, &["", "ab", "", "c"][..]
)]
// No flush hints on the header-less paths, but batches still split at waits.
#[case::no_hints_without_header_first(
    vec![Step::Wait, Step::Data(b"a"), Step::Wait, Step::Data(b"c")], false, None,
    StreamOutcome::Complete, &["a", "c"][..]
)]
// A second wait with nothing new delivered does not flush again.
#[case::flush_only_when_unflushed(
    vec![Step::Wait, Step::Wait, Step::Data(b"a")], true, None,
    StreamOutcome::Complete, &["", "a"][..]
)]
// A body error delivers what was produced, then reports the truncation.
#[case::body_error_delivers_batch(
    vec![Step::Data(b"a"), Step::Fail], true, None,
    StreamOutcome::BodyError, &["a"][..]
)]
#[case::stop_on_batch_before_body_error(
    vec![Step::Data(b"a"), Step::Fail], true, Some(1),
    StreamOutcome::SinkStopped, &["a"][..]
)]
#[case::stop_on_flush(
    vec![Step::Wait, Step::Data(b"a")], true, Some(1),
    StreamOutcome::SinkStopped, &[""][..]
)]
#[case::stop_on_batch_before_wait(
    vec![Step::Data(b"a"), Step::Wait, Step::Data(b"b")], true, Some(1),
    StreamOutcome::SinkStopped, &["a"][..]
)]
#[case::stop_on_final_batch(
    vec![Step::Data(b"a")], true, Some(1),
    StreamOutcome::SinkStopped, &["a"][..]
)]
#[tokio::test]
async fn ready_frames_are_batched_and_waits_flush(
    #[case] steps: Vec<Step>,
    #[case] flush_hints: bool,
    #[case] break_at: Option<usize>,
    #[case] expected_outcome: StreamOutcome,
    #[case] expected_calls: &[&str],
) {
    let (outcome, calls) = scripted(steps, flush_hints, break_at).await;
    assert_eq!(outcome, expected_outcome);
    assert_eq!(calls, expected_calls);
}

/// A batch never grows past the streaming chunk size: a frame that would
/// overflow it delivers the batch first (and a sink stop there is honoured).
#[rstest::rstest]
#[case::continue_(None, StreamOutcome::Complete, 2)]
#[case::stop_on_overflow_delivery(Some(1), StreamOutcome::SinkStopped, 1)]
#[tokio::test]
async fn batch_is_capped_at_the_chunk_size(
    #[case] break_at: Option<usize>,
    #[case] expected_outcome: StreamOutcome,
    #[case] expected_calls: usize,
) {
    static BIG: [u8; crate::DEFAULT_STREAMING_CHUNK_BYTES] =
        [b'x'; crate::DEFAULT_STREAMING_CHUNK_BYTES];
    let half = &BIG[..=(crate::streaming_chunk_bytes() / 2)];
    let (outcome, calls) = scripted(vec![Step::Data(half), Step::Data(half)], true, break_at).await;
    assert_eq!(outcome, expected_outcome);
    assert_eq!(calls.len(), expected_calls);
    assert!(calls.iter().all(|c| c.len() == half.len()));
}
