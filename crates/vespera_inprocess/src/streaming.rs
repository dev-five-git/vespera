//! Streaming dispatch variants: response streaming, header-callback
//! streaming, and bidirectional (request + response) streaming.

use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use axum::body::Body;
use bytes::Bytes;
use http_body::Body as _;
use http_body_util::BodyExt;

use crate::bounded::{Deadline, Prefetched422, prefetch_422_body, timeout_wire};
use crate::dispatch::{cap_and_split, parse_validate_resolve};
use crate::internal::{BODY_SINK_STOPPED_MSG, BODY_STREAM_ERROR_MSG, dispatch_and_split};
use crate::wire::{
    WIRE_HEADER_RESERVE, build_wire_header_bytes, build_wire_header_bytes_hoisting, error_wire,
    split_wire_request,
};

mod producer;
pub use producer::REQUEST_PRODUCER_JOIN_GRACE;
use producer::{ChannelBody, RequestProducerHandle, RequestSourceCloser, join_request_producer};

/// Outcome of one request-body pull on the bidirectional streaming
/// path (the `pull_chunk` callback).
///
/// `Data(empty)` means "nothing right now, keep the stream open" — it
/// is skipped, not treated as EOF.  [`RequestChunk::Error`] terminates
/// the request body with a [`StreamAbort`] so axum and the handler see
/// a failed body rather than a clean EOF — a truncated upload (e.g. the
/// source `InputStream` threw mid-stream) is never silently accepted as
/// complete.
pub enum RequestChunk {
    /// A request body chunk (an empty vec is a no-op "keep open" signal).
    Data(Vec<u8>),
    /// Clean end of the request body.
    End,
    /// The producer failed; the request body errors out instead of
    /// ending cleanly.
    Error,
}

/// Upper bound on consecutive empty request-body pulls before the
/// producer aborts the stream.  A conformant blocking `InputStream`
/// never returns 0 for a non-empty buffer, so sustained empty reads
/// indicate a stuck or hostile producer; the cap stops a DoS busy-spin
/// on a blocking-pool thread.
const MAX_CONSECUTIVE_EMPTY_READS: u32 = 1024;

/// Error yielded by the request body when the producer reports
/// [`RequestChunk::Error`].  Surfaced to axum so a truncated upload is
/// not mistaken for a complete one.
#[derive(Debug)]
pub struct StreamAbort;

impl std::fmt::Display for StreamAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request body stream aborted by producer")
    }
}

impl std::error::Error for StreamAbort {}

/// **Streaming** sibling of [`crate::dispatch_from_bytes_async`].
///
/// Drives the dispatch end-to-end like the non-streaming variant but
/// emits the response body **chunk-by-chunk via `on_chunk`** instead
/// of materialising it in a single `Vec<u8>`.  Returns the wire-format
/// header bytes only (`[u32 BE header_len | header JSON]`) — the body
/// is delivered through the callback while the dispatch is in flight,
/// so a 1 GiB response is never resident in memory.
///
/// # Header ordering (important)
///
/// The returned header bytes become available only **after** the body has
/// been fully drained through `on_chunk`: the status + headers are read off
/// the response after its body stream completes.  This variant therefore
/// suits sinks that buffer the body, or callers that can backfill the
/// status/headers afterwards (the JNI `dispatchStreaming` bridge returns the
/// header to Java only once the native call returns).  Callers that must
/// commit the response status/headers **before** the first body byte — e.g.
/// a Spring `HttpServletResponse` controller streaming straight to the
/// client — MUST instead use [`dispatch_streaming_with_header_async`], which
/// fires a dedicated header callback before any `on_chunk` invocation.
///
/// `on_chunk` is invoked one or more times in arrival order; the
/// borrowed slice is valid only for the duration of each call and the
/// callback should treat it as ephemeral (e.g. write it to an
/// `OutputStream`, accumulate it on disk, …).
///
/// Failure modes are identical to [`crate::dispatch_from_bytes_async`] —
/// returns a valid wire-format error response (header + body) when
/// the wire input is malformed, the version is wrong, no app is
/// registered, or the handler reports a pre-dispatch error.  In the
/// error path the body is included inside the returned bytes (not
/// streamed via `on_chunk`) because the error message is small.
///
/// `on_chunk` is NOT called if the response body is empty.
///
/// `422` responses hoist `validation_errors` into the returned header
/// exactly like the buffered path (a small 422 body is prefetched first;
/// an oversized one is streamed unhoisted).
///
/// A failure **after** body bytes were already delivered through
/// `on_chunk` (body error, sink stop, or an expired
/// [`crate::dispatch_timeout`]) returns a `5xx` error header — the delivered
/// body is truncated.  Callers that must distinguish "clean error response"
/// from "truncated body" (e.g. to abort the transport) should use
/// [`dispatch_streaming_with_outcome_async`].
pub async fn dispatch_streaming_async<F>(input: Vec<u8>, on_chunk: F) -> Vec<u8>
where
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    dispatch_streaming_with_outcome_async(input, on_chunk)
        .await
        .0
}

/// [`dispatch_streaming_async`] that also reports whether the delivered
/// body was truncated.
///
/// Returns `(header_bytes, outcome)`:
///
/// * [`StreamOutcome::Complete`] — `header_bytes` is authoritative: either
///   the success header (body fully delivered via `on_chunk`) or a complete
///   error response delivered **before** any `on_chunk` call (pre-dispatch
///   failure, `413`, `504` before the header, a body error before the
///   first chunk, …).
/// * [`StreamOutcome::BodyError`] / [`StreamOutcome::SinkStopped`] — the
///   failure happened AFTER at least one
///   `on_chunk` call: the delivered body is truncated and the host must
///   abort its transport.  `header_bytes` is a `5xx` error header kept for
///   callers that still want one.
pub async fn dispatch_streaming_with_outcome_async<F>(
    input: Vec<u8>,
    mut on_chunk: F,
) -> (Vec<u8>, StreamOutcome)
where
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    traced!("streaming", {
        let mut deadline = Deadline::start();
        // Response streaming still buffers the full REQUEST in memory
        // (`input` is a complete `Vec`), so it gets the same ingress cap as
        // the buffered entry points.  Only *bidirectional* streaming, which
        // pulls the request body chunk-by-chunk, is exempt.
        let (header_bytes, body_bytes) = match cap_and_split(input) {
            Ok(parts) => parts,
            Err(wire) => return (wire, StreamOutcome::Complete),
        };
        let (header, router) = match parse_validate_resolve(&header_bytes) {
            Ok(parts) => parts,
            Err(wire) => return (wire, StreamOutcome::Complete),
        };
        let default_json_when_absent = !body_bytes.is_empty();
        let dispatched = deadline
            .run(dispatch_and_split(
                router,
                &header.method,
                &header.path,
                &header.query,
                None,
                header.iter_str_pairs(),
                // Owned wire path: share `header_bytes` so plain-value
                // `HeaderValue`s are constructed zero-copy.
                Some(&header_bytes),
                Body::from(body_bytes),
                default_json_when_absent,
                header.connection_info(),
            ))
            .await;
        let (status, headers, metadata, body) = match dispatched {
            None => return (timeout_wire(), StreamOutcome::Complete),
            Some(Err((status, msg))) => return (error_wire(status, &msg), StreamOutcome::Complete),
            Some(Ok(parts)) => parts,
        };

        // Reuse the header-first tail with a capturing header sink: the header
        // (with 422 hoisting) is produced once, and a counting chunk sink tells
        // a pre-chunk failure (clean error response) from a truncation.
        let mut captured: Vec<u8> = Vec::new();
        let mut delivered = false;
        let outcome = {
            let mut on_header = |h: &[u8]| captured.extend_from_slice(h);
            let mut counting = |chunk: &[u8]| {
                delivered = true;
                on_chunk(chunk)
            };
            emit_header_then_stream_body(
                status,
                headers,
                metadata,
                body,
                &mut deadline,
                &mut on_header,
                &mut counting,
                false,
            )
            .await
        };
        match outcome {
            StreamOutcome::Complete => (captured, StreamOutcome::Complete),
            failed => (
                failed.error_wire(),
                if delivered {
                    failed
                } else {
                    StreamOutcome::Complete
                },
            ),
        }
    })
}

/// Outcome of a **header-first** streaming dispatch
/// (`dispatch_streaming_with_header_async`,
/// `dispatch_bidirectional_streaming_with_header*`).
///
/// These functions commit the response header (`on_header`) **before**
/// the body is drained, so a failure that happens afterwards can no
/// longer be turned into an error status. This value surfaces that
/// failure to the host so it can abort the transport (drop the
/// connection / skip the clean chunked terminator) instead of letting a
/// truncated body masquerade as a complete `2xx` response.
///
/// Dropping this value silently turns a `BodyError` / `SinkStopped`
/// (truncated response) into what looks like a clean `Complete`,
/// erasing the abort signal the FFI bridge relies on — hence
/// `#[must_use]`.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamOutcome {
    /// The response body drained to clean EOF — every chunk delivered,
    /// or the dispatch failed *before* the header was committed (the
    /// error response was delivered in full via `on_header`).
    Complete,
    /// The response body stream errored **after** the header was
    /// committed; the bytes delivered via `on_chunk` are truncated.
    BodyError,
    /// `on_chunk` returned [`ControlFlow::Break`] — the chunk sink asked
    /// to stop early (e.g. the host's output sink failed mid-stream).
    /// The response delivered via `on_chunk` is truncated.
    SinkStopped,
}

impl StreamOutcome {
    /// Whether the delivered response body is truncated (anything but
    /// [`StreamOutcome::Complete`]) and the host must abort its transport.
    #[must_use]
    pub const fn is_truncated(self) -> bool {
        !matches!(self, Self::Complete)
    }

    /// Canonical error wire response for a truncated outcome (`500` for a
    /// body error / sink stop); for `Complete` this is never consulted by
    /// the dispatchers and returns a `500` as well.
    #[must_use]
    pub fn error_wire(self) -> Vec<u8> {
        match self {
            Self::SinkStopped => error_wire(500, BODY_SINK_STOPPED_MSG),
            Self::Complete | Self::BodyError => error_wire(500, BODY_STREAM_ERROR_MSG),
        }
    }
}

/// Shared tail of every streaming variant ([`dispatch_streaming_with_header_async`],
/// [`dispatch_streaming_with_outcome_async`] and `bidirectional_streaming_inner`):
/// emit the wire-format response header via `on_header`, then deliver the
/// response body via `on_chunk`.
///
/// On the **422 path** the (small, framework-generated) validation body is
/// prefetched up front — at most `MAX_HOIST_BODY_BYTES + 1` bytes — so its
/// errors are hoisted into the wire header, the same contract the buffered
/// [`crate::wire::to_wire_bytes`] path upholds, so a Java/FFI decoder reads
/// validation failures from the header in EVERY dispatch mode.  The body is
/// still delivered verbatim via `on_chunk`.  Because the prefetch runs
/// *before* the header is committed, a body error (or an expired deadline)
/// there cleanly becomes a `500` (`504`) via `on_header` with nothing
/// truncated.  An oversized 422 skips the hoist and streams like any other
/// status (the already-read prefix first).
///
/// Every other status: a hoist-free header followed by frame-by-frame body
/// streaming (so a 1 GiB response is never resident), with a post-commit
/// body error / sink stop surfaced via the returned [`StreamOutcome`].
///
/// `deadline` bounds only the time to the header (the 422 prefetch above);
/// once the header is committed the body drains without a total deadline.
///
/// Frames that are already available are coalesced (up to the streaming
/// chunk size) into one `on_chunk` call; the batch is delivered as soon as
/// the body would wait.  With `flush_hints` (the header-first variants,
/// whose host has already committed the status) an **empty** `on_chunk`
/// call follows whenever the body is about to wait and something was
/// delivered since the last hint: the host should flush its output buffer
/// then, so incremental responses (SSE) are not held back.
#[expect(
    clippy::too_many_arguments,
    reason = "shared tail of four public streaming entry points; each argument is a distinct \
              per-dispatch input"
)]
async fn emit_header_then_stream_body<H, F>(
    status: u16,
    headers: http::HeaderMap,
    metadata: crate::envelope::ResponseMetadata,
    mut body: Body,
    deadline: &mut Deadline,
    on_header: &mut H,
    on_chunk: &mut F,
    flush_hints: bool,
) -> StreamOutcome
where
    H: FnMut(&[u8]),
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    if status == 422 {
        match deadline.run(prefetch_422_body(body)).await {
            None => {
                on_header(&timeout_wire());
                return StreamOutcome::Complete;
            }
            Some(Prefetched422::Failed) => {
                on_header(&error_wire(500, BODY_STREAM_ERROR_MSG));
                return StreamOutcome::Complete;
            }
            Some(Prefetched422::Small(collected)) => {
                on_header(&build_wire_header_bytes_hoisting(
                    status, &headers, &metadata, &collected,
                ));
                if !collected.is_empty() && on_chunk(&collected).is_break() {
                    return StreamOutcome::SinkStopped;
                }
                return StreamOutcome::Complete;
            }
            Some(Prefetched422::Large(rest)) => body = rest,
        }
    }

    on_header(&build_wire_header_bytes(status, &headers, &metadata));
    // The header is committed: the dispatch deadline bounds only the time to
    // the first header, never the body (a long download that is still making
    // progress must not be cut off mid-stream).
    stream_body_tail(body, on_chunk, flush_hints).await
}

/// Post-header body tail shared by every header-first streaming path and
/// [`crate::PendingDirectResponse::stream`]: frame-by-frame delivery with
/// ready-frame coalescing and (with `flush_hints`) an empty-chunk flush hint
/// before the body waits.  The header was just delivered, so it starts
/// "unflushed".
pub async fn stream_body_tail<F>(
    mut body: Body,
    on_chunk: &mut F,
    flush_hints: bool,
) -> StreamOutcome
where
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    let mut batch = FrameBatch::new(crate::config::streaming_chunk_bytes());
    // Something (the header, then body bytes) reached the sink since the
    // last flush hint.
    let mut unflushed = flush_hints;
    loop {
        // Poll the body ONCE without waiting: frames that are already
        // available are coalesced into one sink call (fewer FFI crossings for
        // bodies made of many small frames), and only when the body would
        // actually wait is the batch delivered — followed by a flush hint on
        // the header-first paths, so a latency-sensitive stream (SSE, chunked
        // progress) reaches the client now instead of sitting in the host's
        // output buffer.  A body that never waits (an in-memory payload) is
        // never flushed early, so bulk throughput is unchanged.
        let next = match poll_frame_now(&mut body).await {
            Poll::Ready(next) => next,
            Poll::Pending => {
                if batch.deliver(on_chunk).is_break() {
                    return StreamOutcome::SinkStopped;
                }
                if unflushed {
                    unflushed = false;
                    if on_chunk(&[]).is_break() {
                        return StreamOutcome::SinkStopped;
                    }
                }
                body.frame().await
            }
        };
        match next {
            None => {
                return if batch.deliver(on_chunk).is_break() {
                    StreamOutcome::SinkStopped
                } else {
                    StreamOutcome::Complete
                };
            }
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data()
                    && !data.is_empty()
                {
                    unflushed = flush_hints;
                    // The chunk sink asked to stop (e.g. the host's output sink
                    // failed). The header is already committed, so report the
                    // truncation to the caller.
                    if batch.push(data, on_chunk).is_break() {
                        return StreamOutcome::SinkStopped;
                    }
                }
            }
            // The response body aborted mid-stream after the header was
            // committed: status/headers can no longer change, so deliver what
            // was already produced and surface the truncation so the host can
            // abort the transport instead of sending a clean terminator over a
            // short body.
            Some(Err(_)) => {
                return if batch.deliver(on_chunk).is_break() {
                    StreamOutcome::SinkStopped
                } else {
                    StreamOutcome::BodyError
                };
            }
        }
    }
}

/// Poll `body` for its next frame exactly once, without waiting: `Pending`
/// means the body has nothing ready right now (its waker is registered, so a
/// following `body.frame().await` resumes normally).
fn poll_frame_now(
    body: &mut Body,
) -> impl Future<Output = Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>>> + '_ {
    std::future::poll_fn(move |cx| Poll::Ready(Pin::new(&mut *body).poll_frame(cx)))
}

/// Coalesces response frames that are ready back-to-back into one sink call.
///
/// The first frame is held zero-copy (a single-frame body — the common
/// `Json` / `Bytes` response — is delivered without any copy); only a
/// second small frame arriving before the body waits starts a copying
/// batch, capped at the streaming chunk size so the buffered amount stays
/// `O(chunk)`.
struct FrameBatch {
    held: Option<Bytes>,
    merged: Vec<u8>,
    cap: usize,
}

impl FrameBatch {
    const fn new(cap: usize) -> Self {
        Self {
            held: None,
            merged: Vec::new(),
            cap,
        }
    }

    fn len(&self) -> usize {
        self.held.as_ref().map_or(self.merged.len(), Bytes::len)
    }

    /// Add one non-empty frame, delivering the current batch first when the
    /// frame would push it past the cap.
    fn push<F>(&mut self, data: Bytes, on_chunk: &mut F) -> ControlFlow<()>
    where
        F: FnMut(&[u8]) -> ControlFlow<()>,
    {
        if self.len().saturating_add(data.len()) > self.cap {
            self.deliver(on_chunk)?;
        }
        if self.len() == 0 {
            self.held = Some(data);
        } else {
            if let Some(held) = self.held.take() {
                self.merged.extend_from_slice(&held);
            }
            self.merged.extend_from_slice(&data);
        }
        ControlFlow::Continue(())
    }

    /// Hand the batched bytes (if any) to the sink.
    fn deliver<F>(&mut self, on_chunk: &mut F) -> ControlFlow<()>
    where
        F: FnMut(&[u8]) -> ControlFlow<()>,
    {
        if let Some(held) = self.held.take() {
            return on_chunk(&held);
        }
        if self.merged.is_empty() {
            return ControlFlow::Continue(());
        }
        let flow = on_chunk(&self.merged);
        self.merged.clear();
        flow
    }
}

/// **Streaming dispatch with explicit header callback** — emits the
/// wire-format response header via `on_header` **before** any body
/// chunk is delivered to `on_chunk`.
///
/// This is the variant Spring `HttpServletResponse`-based controllers
/// want: `on_header` fires while the response is still uncommitted,
/// so the controller can call `resp.setStatus(...)` /
/// `resp.setHeader(...)` from the callback. Then `on_chunk` streams
/// the body bytes one frame at a time.
///
/// `on_header` is called **exactly once** in every code path —
/// success or error. On error (malformed wire, no app, invalid
/// method, …) the bytes passed to `on_header` are a normal
/// `error_wire(...)` response and `on_chunk` is **not** invoked.
///
/// Body frames that are ready back-to-back are coalesced (up to
/// [`crate::streaming_chunk_bytes`]) into one `on_chunk` call.  `on_chunk`
/// may also be called with an **empty slice**: a *flush hint* fired when the
/// body is about to wait after something (the header or body bytes) was
/// delivered — a buffering sink (e.g. a servlet `OutputStream`) should flush
/// so incremental responses such as `text/event-stream` reach the client
/// immediately.  Sinks that do not buffer can ignore it.
pub async fn dispatch_streaming_with_header_async<H, F>(
    input: Vec<u8>,
    mut on_header: H,
    mut on_chunk: F,
) -> StreamOutcome
where
    H: FnMut(&[u8]),
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    traced!("streaming", {
        let mut deadline = Deadline::start();
        // Response streaming buffers the full request (see
        // `dispatch_streaming_async`): apply the ingress cap, delivering the
        // 413 through the header callback so the contract (header fires
        // exactly once) holds.  Pre-header error paths return `Complete`: the
        // (error) response was delivered in full via `on_header`, nothing is
        // truncated.
        let (header_bytes, body_bytes) = match cap_and_split(input) {
            Ok(parts) => parts,
            Err(wire) => {
                on_header(&wire);
                return StreamOutcome::Complete;
            }
        };
        let (header, router) = match parse_validate_resolve(&header_bytes) {
            Ok(parts) => parts,
            Err(wire) => {
                on_header(&wire);
                return StreamOutcome::Complete;
            }
        };

        // Content-Type defaulting (INP-03): a non-empty body with no explicit
        // `Content-Type` defaults to `application/json`.  dispatch_and_split
        // detects the header during its build pass, so this variant stays
        // identical to its siblings while skipping the separate pre-scan; we
        // signal only that a non-empty body should default.  Computed before
        // `body_bytes` is moved.
        let default_json_when_absent = !body_bytes.is_empty();
        // Streaming is dominated by body throughput, so the owned-path URI
        // zero-copy is not worth threading here — pass `None` (the URI is parsed
        // from the borrowed path by `build_uri`, exactly as before).  Header
        // values, however, are still inserted into the request `HeaderMap` per
        // request — share `header_bytes` so each plain value is constructed
        // zero-copy via `HeaderValue::from_maybe_shared`.
        let (status, headers, metadata, body) = match deadline
            .run(dispatch_and_split(
                router,
                &header.method,
                &header.path,
                &header.query,
                None,
                header.iter_str_pairs(),
                Some(&header_bytes),
                Body::from(body_bytes),
                default_json_when_absent,
                header.connection_info(),
            ))
            .await
        {
            Some(Ok(parts)) => parts,
            Some(Err((status, msg))) => {
                on_header(&error_wire(status, &msg));
                return StreamOutcome::Complete;
            }
            // Deadline expired before the header was committed: a clean 504.
            None => {
                on_header(&timeout_wire());
                return StreamOutcome::Complete;
            }
        };

        emit_header_then_stream_body(
            status,
            headers,
            metadata,
            body,
            &mut deadline,
            &mut on_header,
            &mut on_chunk,
            true,
        )
        .await
    })
}

/// **Bidirectional streaming dispatch** — both request and response
/// bodies are streamed chunk-by-chunk; neither side materialises the
/// full payload in memory.
///
/// - `input_header` is a wire-format request **without a body**
///   (just `[u32 BE header_len | JSON header]`).  Send the body
///   chunks via `pull_chunk`, not embedded in this buffer.
/// - `pull_chunk` is called repeatedly to obtain request body
///   chunks.  Return [`RequestChunk::Data`] for each chunk and
///   [`RequestChunk::End`] to signal clean EOF.  An empty
///   `Data(Vec::new())` is treated as "no more data right now, but
///   keep the stream open" — rarely useful; most callers should just
///   return `End`.  Return [`RequestChunk::Error`] to abort the
///   request body (e.g. the source stream threw) so the truncated
///   upload is rejected rather than seen as complete.
/// - `on_chunk` receives response body chunks in arrival order, same
///   contract as [`dispatch_streaming_async`].
///
/// Returns the wire-format **header only** (`[u32 BE header_len |
/// header JSON]`) — the response body was delivered via `on_chunk`.
///
/// `pull_chunk` runs on a Tokio blocking thread (`spawn_blocking`)
/// because the JNI implementation reads from a Java `InputStream`,
/// which is inherently blocking.  That blocking producer is started
/// lazily on the first request-body poll, so handlers that never read
/// the body never touch the `InputStream`. Backpressure is enforced by
/// a bounded mpsc channel ([`crate::effective_streaming_channel_capacity`] slots,
/// default 16): if axum reads slowly, the `pull_chunk` call blocks
/// naturally.
///
/// Failure modes match [`dispatch_streaming_async`]: malformed
/// header / unknown version / no app / handler error → normal
/// `error_wire(...)` response (with the message inside the returned
/// bytes); neither callback is invoked in those paths.
///
/// This is the ergonomic form with **no request-source close hook** —
/// the request producer is awaited (for at most
/// [`REQUEST_PRODUCER_JOIN_GRACE`], then detached).  Callers
/// with a blocking request source that can park forever (e.g. a Java
/// `InputStream` that never reaches EOF) should use
/// [`dispatch_bidirectional_streaming_closing`] to supply a close hook.
pub async fn dispatch_bidirectional_streaming<P, F>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
) -> Vec<u8>
where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    dispatch_bidirectional_streaming_closing(input_header, pull_chunk, on_chunk, || {}).await
}

/// **Bidirectional streaming with a request-source close hook** — the
/// [`dispatch_bidirectional_streaming`] variant that takes a
/// `request_close` callback.
///
/// `request_close` is invoked once, after the response body is fully
/// drained, **only if** the request producer was started (the handler
/// read at least one body chunk).  It must close/abort the request body
/// source (e.g. the Java `InputStream`) so a producer parked in a
/// blocking read is unblocked and this call cannot hang on a stuck upload
/// that never reaches EOF.  It is a no-op for full reads (already at EOF)
/// and is never called when the handler ignored the body.
pub async fn dispatch_bidirectional_streaming_closing<P, F, C>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
    request_close: C,
) -> Vec<u8>
where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]) -> ControlFlow<()>,
    C: FnOnce(),
{
    let mut header_bytes: Vec<u8> = Vec::with_capacity(4 + WIRE_HEADER_RESERVE);
    let outcome = {
        let on_header = |h: &[u8]| header_bytes.extend_from_slice(h);
        bidirectional_streaming_inner(
            input_header,
            pull_chunk,
            on_chunk,
            on_header,
            request_close,
            false,
        )
        .await
    };
    match outcome {
        // `Complete` covers a clean drain AND the pre-dispatch error paths
        // (which deliver a full `error_wire(...)` via `on_header`), so the
        // captured bytes are authoritative.
        StreamOutcome::Complete => header_bytes,
        // The response body errored or the chunk sink stopped AFTER the
        // success header was captured into `header_bytes` —
        // the delivered body is truncated.  Replace the captured success
        // header with a 5xx so a truncated bidirectional response is never
        // returned as a clean success (mirrors `dispatch_streaming_async`).
        failed => failed.error_wire(),
    }
}

/// **Bidirectional streaming with explicit header callback** — the
/// `with_header` counterpart of [`dispatch_bidirectional_streaming`].
/// Emits the wire-format response header via `on_header` **before**
/// any response body byte reaches `on_chunk`, so Spring-style
/// `HttpServletResponse` controllers can commit status / headers
/// from the callback while the response is still uncommitted.
///
/// `on_header` is called exactly once on every code path (success or
/// error). On any pre-dispatch / wire error the bytes passed to
/// `on_header` are a normal `error_wire(...)` response and neither
/// `pull_chunk` nor `on_chunk` is invoked beyond that point.
///
/// Like [`dispatch_streaming_with_header_async`], ready frames are
/// coalesced and `on_chunk(&[])` is a flush hint fired before the body
/// waits.
///
/// Ergonomic form with no request-source close hook; see
/// [`dispatch_bidirectional_streaming_with_header_closing`] for the
/// variant that supplies one.
pub async fn dispatch_bidirectional_streaming_with_header<P, F, H>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
    on_header: H,
) -> StreamOutcome
where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]) -> ControlFlow<()>,
    H: FnMut(&[u8]),
{
    dispatch_bidirectional_streaming_with_header_closing(
        input_header,
        pull_chunk,
        on_chunk,
        on_header,
        || {},
    )
    .await
}

/// **Bidirectional streaming with header callback and request-source
/// close hook** — the [`dispatch_bidirectional_streaming_with_header`]
/// variant that takes a `request_close` callback (see
/// [`dispatch_bidirectional_streaming_closing`] for its contract).
pub async fn dispatch_bidirectional_streaming_with_header_closing<P, F, H, C>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
    on_header: H,
    request_close: C,
) -> StreamOutcome
where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]) -> ControlFlow<()>,
    H: FnMut(&[u8]),
    C: FnOnce(),
{
    bidirectional_streaming_inner(
        input_header,
        pull_chunk,
        on_chunk,
        on_header,
        request_close,
        true,
    )
    .await
}

async fn bidirectional_streaming_inner<P, F, H, C>(
    input_header: Vec<u8>,
    pull_chunk: P,
    mut on_chunk: F,
    mut on_header: H,
    request_close: C,
    flush_hints: bool,
) -> StreamOutcome
where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]) -> ControlFlow<()>,
    H: FnMut(&[u8]),
    C: FnOnce(),
{
    traced!("bidirectional", {
        let (header_bytes, body_tail) = match split_wire_request(input_header) {
            Ok(parts) => parts,
            Err(msg) => {
                on_header(&error_wire(400, &msg));
                return StreamOutcome::Complete;
            }
        };
        // `input_header` MUST be header-only on the bidirectional path — the
        // request body arrives via `pull_chunk`.  A non-empty tail means the
        // caller mis-built the frame; reject it (400) instead of silently
        // retaining (then discarding) a full body allocation, which would also
        // violate the advertised O(chunk) memory contract.
        if !body_tail.is_empty() {
            on_header(&error_wire(
                400,
                "bidirectional streaming input_header must be header-only \
                 (no trailing body bytes); send the request body via pull_chunk",
            ));
            return StreamOutcome::Complete;
        }
        let (header, router) = match parse_validate_resolve(&header_bytes) {
            Ok(parts) => parts,
            Err(wire) => {
                on_header(&wire);
                return StreamOutcome::Complete;
            }
        };

        let mut deadline = Deadline::start();
        let max_request_body = crate::config::max_streaming_request_bytes();
        let limit_exceeded = Arc::new(AtomicBool::new(false));
        let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(None));
        let body = Body::new(ChannelBody::new(
            pull_chunk,
            Arc::clone(&producer_handle),
            max_request_body,
            Arc::clone(&limit_exceeded),
        ));
        // RAII guard: closes the request source iff the producer was started, on
        // EVERY exit path — including a panic unwinding out of the handler or out
        // of the response-body poll below. Without it, a handler that read part of
        // the body (starting the producer) and then panicked would leave the
        // producer parked forever in a blocking source read: the JNI boundary's
        // `catch_unwind` turns the panic into a 500 but skips the explicit close,
        // so the parked producer never gets unblocked. This is the panic-path
        // sibling of the M3 hang.
        let mut closer = RequestSourceCloser::new(Arc::clone(&producer_handle), request_close);

        // Content-Type parity with the buffered / direct / response-streaming
        // paths: a request with no explicit Content-Type defaults to
        // `application/json`.  The streamed body's emptiness is unknowable up
        // front (unlike the buffered paths, which gate on a non-empty body), so
        // default whenever the header is absent — matching sibling behaviour for
        // the bodyful bidirectional requests that are this path's reason to
        // exist, instead of leaving extractor behaviour mode-dependent.
        let default_json_when_absent = true;
        // See the response-streaming sibling: streaming is body-throughput bound,
        // so pass `None` rather than threading the owned-path URI zero-copy here.
        // Share `header_bytes` for the per-request `HeaderValue` insertions so
        // each plain value is constructed zero-copy.
        let dispatched = deadline
            .run(dispatch_and_split(
                router,
                &header.method,
                &header.path,
                &header.query,
                None,
                header.iter_str_pairs(),
                Some(&header_bytes),
                body,
                default_json_when_absent,
                header.connection_info(),
            ))
            .await;
        // Pre-commit failures (bad method/path → 405/400, expired deadline →
        // 504, request body over `max_streaming_request_bytes` → 413): close the
        // request source (no-op if the producer never started), join the
        // producer (bounded), then deliver one complete error header.
        let (status, headers, metadata, response_body) = match dispatched {
            Some(Ok(parts)) if !limit_exceeded.load(Ordering::Acquire) => parts,
            failed => {
                let wire = match failed {
                    None => timeout_wire(),
                    Some(Err((status, msg))) => error_wire(status, &msg),
                    Some(Ok(_)) => error_wire(
                        413,
                        &format!(
                            "streaming request body exceeds configured maximum of \
                             {max_request_body} bytes (VESPERA_MAX_STREAMING_REQUEST_BYTES)"
                        ),
                    ),
                };
                closer.close_if_started();
                join_request_producer(&producer_handle).await;
                on_header(&wire);
                return StreamOutcome::Complete;
            }
        };

        let outcome = emit_header_then_stream_body(
            status,
            headers,
            metadata,
            response_body,
            &mut deadline,
            &mut on_header,
            &mut on_chunk,
            flush_hints,
        )
        .await;

        // The response is fully drained, so the handler has finished and will
        // not read more of the request body. If the producer was started (the
        // handler read at least one chunk) it may be parked in a blocking source
        // read; close the request source to unblock it. The join below is
        // BOUNDED (`REQUEST_PRODUCER_JOIN_GRACE`): a host source whose close does
        // not interrupt a read parked on another thread can no longer hang the
        // completed response — the producer is detached instead.  A full read
        // already hit EOF (close is a no-op) and a producer that never started
        // leaves the source untouched. `close_if_started` is idempotent, so the
        // guard's Drop becomes a no-op on this happy path.
        closer.close_if_started();
        join_request_producer(&producer_handle).await;
        outcome
    })
}

#[cfg(test)]
mod tests;
