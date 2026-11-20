use super::inline_pull::InlineState;
use super::{
    ChannelBody, Delivery, RequestProducerHandle, RequestSourceCloser, StreamOutcome,
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

    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hook_runs = Arc::clone(&runs);
    let mut closer = RequestSourceCloser::new(
        Arc::clone(&producer_handle),
        InlineState::new(false),
        move || {
            hook_runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("hook boom");
        },
    );
    // Returns normally — the panic is caught inside `close_if_started`.
    closer.close_if_started();
    // The hook really ran (and panicked): a skipped hook must not pass.
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Idempotent: the hook was consumed on the first call, so this is a
    // no-op and does not panic a second time.
    closer.close_if_started();
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
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
        Delivery::Captured,
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
        Delivery::Captured,
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
        Delivery::Captured,
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
        Delivery::Captured,
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
        slots: Some(Arc::new(tokio::sync::Semaphore::new(1))),
        permit_wait: None,
        inline_budget: 0,
        inline_pending: Bytes::new(),
        inline: InlineState::new(false),
        failed: false,
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
        // Closed inline gate: these tests exercise the spawned producer.
        InlineState::new(false),
    )
}

/// While the producer has not sent anything, the body reports `Pending` (the
/// consumer waits; no frame, no end). The producer is released only after
/// that is observed, so the check is deterministic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_body_is_pending_while_the_producer_is_slow() {
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let mut sent = false;
    let mut body = channel_body(move || {
        if sent {
            crate::RequestChunk::End
        } else {
            sent = true;
            wait.recv().ok();
            crate::RequestChunk::Data(b"abc".to_vec())
        }
    });
    let pending = std::future::poll_fn(|cx| {
        let polled = Pin::new(&mut body).poll_frame(cx);
        Poll::Ready(polled.is_pending())
    })
    .await;
    assert!(pending, "an empty, still-open channel must be Pending");
    go.send(()).expect("producer waiting");
    let data = body.frame().await.expect("frame").expect("data");
    assert_eq!(data.into_data().expect("data frame"), "abc");
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
        slots: Some(Arc::new(tokio::sync::Semaphore::new(1))),
        permit_wait: None,
        inline_budget: 0,
        inline_pending: Bytes::new(),
        inline: InlineState::new(false),
        failed: false,
    };
    let first = body.frame().await;
    assert_eq!(matches!(first, Some(Err(_))), expect_abort);
    assert_eq!(first.is_none(), !expect_abort);
    // A failed body keeps failing on every later poll (it must never read as
    // a clean EOF); a cleanly ended one stays at EOF.
    let second = body.frame().await;
    assert_eq!(
        matches!(second, Some(Err(_))),
        expect_abort,
        "the state after the first close is sticky"
    );
    assert_eq!(second.is_none(), !expect_abort);
}

/// Every failure latches: polling the body again after the error must keep
/// failing, not report a clean EOF (a consumer that ignores the first error
/// and polls on would otherwise accept a truncated upload as complete).
#[rstest::rstest]
#[case::producer_error(0)]
#[case::request_cap(1)]
#[case::refused_start(2)]
#[case::inline_error(3)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_body_keeps_failing_on_every_poll(#[case] which: u8) {
    let mut body = match which {
        // Producer reports `Error` on its first pull.
        0 => channel_body(|| crate::RequestChunk::Error),
        // Producer delivers two bytes against a cap of one.
        1 => {
            let mut sent = false;
            let mut body = ChannelBody::new(
                move || {
                    if sent {
                        crate::RequestChunk::End
                    } else {
                        sent = true;
                        crate::RequestChunk::Data(b"ab".to_vec())
                    }
                },
                Arc::new(Mutex::new(None)),
                1,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                InlineState::new(false),
            );
            body.capacity = 1;
            body
        }
        // The dispatch finished before the producer could start.
        2 => {
            let inline = InlineState::new(false);
            inline.finish();
            ChannelBody::new(
                || crate::RequestChunk::End,
                Arc::new(Mutex::new(None)),
                0,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                inline,
            )
        }
        // Inline pull reports `Error` (multi-thread `block_on`, no task id).
        _ => {
            let mut body = ChannelBody::new(
                || crate::RequestChunk::Error,
                Arc::new(Mutex::new(None)),
                0,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                InlineState::new(true),
            );
            body.inline_budget = 2;
            body
        }
    };
    for _ in 0..3 {
        assert!(
            matches!(body.frame().await, Some(Err(_))),
            "a failed body must keep failing (case {which})"
        );
    }
}

/// A header-first pre-header error whose body the sink refuses reports the
/// truncation (`SinkStopped`), with the header already delivered.
#[test]
fn pre_header_error_body_sink_stop_is_reported() {
    let wire = crate::wire::error_wire(400, "bad request");
    let mut header = Vec::new();
    let outcome = super::pre_header_error(
        Delivery::HeaderFirst,
        &mut |bytes: &[u8]| header.extend_from_slice(bytes),
        &mut |_: &[u8]| ControlFlow::Break(()),
        &wire,
    );
    assert_eq!(outcome, StreamOutcome::SinkStopped);
    assert_eq!(decode_status(&header), 400);
}

/// A `HeaderFirst` pre-header error sends its header through `on_header`
/// and its message through `on_chunk`, matching the framing of a success.
#[tokio::test]
async fn validation_body_error_sends_its_message_through_chunks() {
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
        Delivery::HeaderFirst,
    )
    .await;

    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(decode_status(&header), 500);
    assert_eq!(chunks, crate::internal::BODY_STREAM_ERROR_MSG.as_bytes());
    let json_len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
    assert_eq!(
        header.len(),
        4 + json_len,
        "the header callback gets no body"
    );
}

/// The producer-slot semaphore admits uncontended producers without
/// boxing, and a saturated cap leaves the body pending until a slot frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producer_slot_waits_while_saturated() {
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let mut first = channel_body(|| crate::RequestChunk::End);
    first.slots = Some(Arc::clone(&slots));
    let held = std::future::poll_fn(|cx| first.poll_producer_slot(cx)).await;
    assert!(held.is_some(), "uncontended slot");
    assert!(first.permit_wait.is_none(), "fast path does not box");

    let mut second = channel_body(|| crate::RequestChunk::End);
    second.slots = Some(Arc::clone(&slots));
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
    third.slots = Some(Arc::clone(&slots));
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
    body.slots = Some(Arc::clone(&slots));
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

/// A body polled for the first time AFTER the dispatch finished (it was
/// moved into a detached task) must never start the producer or call the
/// host source: it fails with `StreamAbort`, and the close hook (nothing was
/// read) is not called either.
#[tokio::test]
async fn body_polled_after_the_dispatch_finished_never_reads_the_source() {
    let pulls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pulls_in_source = Arc::clone(&pulls);
    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(None));
    let inline = InlineState::new(false);
    let mut body = ChannelBody::new(
        move || {
            pulls_in_source.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::RequestChunk::End
        },
        Arc::clone(&producer_handle),
        0,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Arc::clone(&inline),
    );
    let hook_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let closes_in_hook = Arc::clone(&hook_calls);
    let mut source_closer =
        RequestSourceCloser::new(Arc::clone(&producer_handle), inline, move || {
            closes_in_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
    source_closer.close_if_started();

    assert!(matches!(body.frame().await, Some(Err(_))));
    assert_eq!(pulls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(producer_handle.lock().unwrap().is_none(), "no producer");
    assert!(
        matches!(body.frame().await, Some(Err(_))),
        "still a failed body after the abort, never a clean EOF"
    );
    drop(source_closer);
    assert_eq!(hook_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// A dispatch that already finished fails its unread body at once, even while
/// every producer slot is held by another upload: the body must not queue
/// behind a slot that may never free (its source is dropped unread).
#[tokio::test]
async fn a_finished_dispatch_does_not_wait_for_a_producer_slot() {
    let pulls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pulls_in_source = Arc::clone(&pulls);
    let inline = InlineState::new(false);
    let mut body = ChannelBody::new(
        move || {
            pulls_in_source.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::RequestChunk::End
        },
        Arc::new(Mutex::new(None)),
        0,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Arc::clone(&inline),
    );
    // Every slot is held by another upload.
    body.slots = Some(Arc::new(tokio::sync::Semaphore::new(0)));
    inline.finish();
    let polled = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame()).await;
    assert!(
        matches!(polled, Ok(Some(Err(_)))),
        "fails at once: {polled:?}"
    );
    assert!(body.permit_wait.is_none(), "no slot was queued for");
    assert_eq!(pulls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// A dispatch that finishes while its body waits for a producer slot fails
/// the body once a slot frees: the producer never starts and the source is
/// never read (the start is refused under the producer-handle lock).
#[tokio::test]
async fn a_dispatch_finished_while_waiting_never_starts_the_producer() {
    let pulls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let pulls_in_source = Arc::clone(&pulls);
    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(None));
    let inline = InlineState::new(false);
    let mut body = ChannelBody::new(
        move || {
            pulls_in_source.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::RequestChunk::End
        },
        Arc::clone(&producer_handle),
        0,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Arc::clone(&inline),
    );
    let slots = Arc::new(tokio::sync::Semaphore::new(0));
    body.slots = Some(Arc::clone(&slots));
    let waiting =
        std::future::poll_fn(|cx| Poll::Ready(body.poll_start_producer(cx).is_pending())).await;
    assert!(waiting, "no slot: the body waits");
    inline.finish();
    slots.add_permits(1);
    let aborted = std::future::poll_fn(|cx| Poll::Ready(body.poll_start_producer(cx))).await;
    assert!(matches!(aborted, Poll::Ready(Err(_))), "{aborted:?}");
    assert!(producer_handle.lock().unwrap().is_none(), "no producer");
    assert!(body.permit_wait.is_none(), "the slot wait is dropped");
    assert_eq!(pulls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(slots.available_permits(), 1, "the freed slot was returned");
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
        if flush_hints {
            Delivery::HeaderFirst
        } else {
            Delivery::Captured
        },
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
// A second wait with nothing new delivered does not flush again: the empty
// data frame between the waits leaves nothing unflushed, so the second
// `Pending` reaches the `unflushed` guard (without it: `["", "", "a"]`).
#[case::flush_only_when_unflushed(
    vec![Step::Wait, Step::Data(b""), Step::Wait, Step::Data(b"a")], true, None,
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
    // Sized from the RESOLVED chunk size (`VESPERA_STREAMING_CHUNK_BYTES`
    // may move it anywhere in 4 KiB..=8 MiB), never a fixed array.
    static HALF: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let half: &'static [u8] =
        HALF.get_or_init(|| vec![b'x'; crate::streaming_chunk_bytes() / 2 + 1]);
    let (outcome, calls) = scripted(vec![Step::Data(half), Step::Data(half)], true, break_at).await;
    assert_eq!(outcome, expected_outcome);
    assert_eq!(calls.len(), expected_calls);
    assert!(calls.iter().all(|c| c.len() == half.len()));
}

/// `send_frame` on a full channel: waits for space (`true` once the
/// receiver makes room), gives up once the dispatch finished (`false`, the
/// frame is dropped), and reports a dropped receiver as `false`.
#[rstest::rstest]
#[case::space_frees(false, true)]
#[case::dispatch_finishes(true, false)]
fn send_frame_waits_for_space_until_the_dispatch_finished(
    #[case] finish: bool,
    #[case] expected: bool,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let inline = InlineState::new(false);
    assert!(super::producer::send_frame(
        &tx,
        Ok(Bytes::from_static(b"a")),
        &inline
    ));
    let sender = {
        let inline = Arc::clone(&inline);
        std::thread::spawn(move || super::producer::send_frame(&tx, Ok(Bytes::new()), &inline))
    };
    // Let the sender park on the full channel first (either order is
    // correct: `send_frame` re-checks after arming its wake-ups).
    std::thread::sleep(std::time::Duration::from_millis(20));
    if finish {
        inline.finish();
    } else {
        assert!(rx.try_recv().is_ok());
    }
    assert_eq!(sender.join().unwrap(), expected);
    let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);
    drop(closed_rx);
    assert!(!super::producer::send_frame(
        &closed_tx,
        Ok(Bytes::new()),
        &inline
    ));
}

/// A channel already full when the dispatch has finished is given up on
/// without parking.
#[test]
fn send_frame_on_a_full_channel_after_finish_gives_up_at_once() {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let inline = InlineState::new(false);
    assert!(super::producer::send_frame(&tx, Ok(Bytes::new()), &inline));
    inline.finish();
    assert!(!super::producer::send_frame(&tx, Ok(Bytes::new()), &inline));
}
