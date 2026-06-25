//! Streaming dispatch variants: response streaming, header-callback
//! streaming, and bidirectional (request + response) streaming.

use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use axum::body::Body;
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;

use crate::config::effective_streaming_channel_capacity;
use crate::dispatch::{check_ingress_cap, parse_validate_resolve};
use crate::internal::{dispatch_and_split, dispatch_response_streaming};
use crate::wire::{
    WIRE_HEADER_RESERVE, build_wire_header_bytes, build_wire_header_bytes_hoisting, error_wire,
    split_wire_request,
};

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

/// **Streaming** sibling of [`dispatch_from_bytes_async`].
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
/// Failure modes are identical to [`dispatch_from_bytes_async`] —
/// returns a valid wire-format error response (header + body) when
/// the wire input is malformed, the version is wrong, no app is
/// registered, or the handler reports a pre-dispatch error.  In the
/// error path the body is included inside the returned bytes (not
/// streamed via `on_chunk`) because the error message is small.
///
/// `on_chunk` is NOT called if the response body is empty.
pub async fn dispatch_streaming_async<F>(input: Vec<u8>, mut on_chunk: F) -> Vec<u8>
where
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    // Response streaming still buffers the full REQUEST in memory
    // (`input` is a complete `Vec`), so it gets the same ingress cap as
    // the buffered entry points.  Only *bidirectional* streaming, which
    // pulls the request body chunk-by-chunk, is exempt.
    if let Some(err) = check_ingress_cap(input.len()) {
        return err;
    }
    let (header_bytes, body_bytes) = match split_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => return error_wire(400, &msg),
    };
    let (header, router) = match parse_validate_resolve(&header_bytes) {
        Ok(parts) => parts,
        Err(wire) => return wire,
    };
    let (status, headers, metadata) = match dispatch_response_streaming(
        router,
        &header.method,
        &header.path,
        &header.query,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        body_bytes,
        // Owned wire path: share `header_bytes` so plain-value
        // `HeaderValue`s are constructed zero-copy via
        // `HeaderValue::from_maybe_shared` (see `dispatch_from_bytes_async`).
        Some(&header_bytes),
        &mut on_chunk,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => return error_wire(status, &msg),
    };
    // Emit header-only wire bytes; body was streamed via on_chunk.
    // NOTE: this header-LAST streaming variant cannot hoist 422 validation
    // errors — the body has already been streamed through on_chunk before the
    // header is built, so it is no longer available to hoist (the caller has
    // received it regardless).  The header-FIRST `*_with_header` variants DO
    // hoist (they buffer the small 422 body before committing the header);
    // callers needing hoisting should use those or dispatch_from_bytes_async.
    build_wire_header_bytes(status, &headers, &metadata)
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

/// Shared tail of the **header-first** streaming variants
/// ([`dispatch_streaming_with_header_async`] and
/// [`bidirectional_streaming_inner`]): emit the wire-format response header via
/// `on_header`, then deliver the response body via `on_chunk`.
///
/// On the **422 path** the (small, framework-generated) validation body is
/// collected up front so its errors are hoisted into the wire header — the same
/// contract the buffered [`crate::wire::to_wire_bytes`] path upholds, so a
/// Java/FFI decoder reads validation failures from the header in EVERY dispatch
/// mode, not just buffered/direct.  The body is still delivered verbatim via
/// `on_chunk`.  Because the 422 body is collected *before* the header is
/// committed, a body error there cleanly becomes a `500` via `on_header` with
/// nothing truncated.
///
/// Every other status keeps the original behaviour exactly: a hoist-free header
/// followed by frame-by-frame body streaming (so a 1 GiB response is never
/// resident), with a post-commit body error / sink stop surfaced via the
/// returned [`StreamOutcome`].
async fn emit_header_then_stream_body<H, F>(
    status: u16,
    headers: http::HeaderMap,
    metadata: crate::envelope::ResponseMetadata,
    mut body: Body,
    on_header: &mut H,
    on_chunk: &mut F,
) -> StreamOutcome
where
    H: FnMut(&[u8]),
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    if status == 422 {
        // Collect the small validation envelope first so it can be hoisted into
        // the header. Collecting BEFORE committing the header means a body
        // error here is a clean 500 (nothing truncated), unlike the post-commit
        // streaming path below.
        let Ok(collected) = body.collect().await else {
            on_header(&error_wire(500, "response body stream error"));
            return StreamOutcome::Complete;
        };
        let collected = collected.to_bytes();
        on_header(&build_wire_header_bytes_hoisting(
            status, &headers, &metadata, &collected,
        ));
        if !collected.is_empty() && on_chunk(&collected).is_break() {
            return StreamOutcome::SinkStopped;
        }
        return StreamOutcome::Complete;
    }

    on_header(&build_wire_header_bytes(status, &headers, &metadata));
    let mut outcome = StreamOutcome::Complete;
    while let Some(frame_result) = body.frame().await {
        if let Ok(frame) = frame_result {
            if let Some(data) = frame.data_ref()
                && !data.is_empty()
                && on_chunk(data.as_ref()).is_break()
            {
                // The chunk sink asked to stop (e.g. the host's output sink
                // failed). The header is already committed, so report the
                // truncation to the caller.
                outcome = StreamOutcome::SinkStopped;
                break;
            }
        } else {
            // The response body aborted mid-stream after the header was
            // committed: status/headers can no longer change, so surface the
            // truncation so the host can abort the transport instead of sending
            // a clean terminator over a short body.
            outcome = StreamOutcome::BodyError;
            break;
        }
    }
    outcome
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
pub async fn dispatch_streaming_with_header_async<H, F>(
    input: Vec<u8>,
    mut on_header: H,
    mut on_chunk: F,
) -> StreamOutcome
where
    H: FnMut(&[u8]),
    F: FnMut(&[u8]) -> ControlFlow<()>,
{
    // Response streaming buffers the full request (see
    // `dispatch_streaming_async`): apply the ingress cap, delivering the
    // 413 through the header callback so the contract (header fires
    // exactly once) holds.  Pre-header error paths return `Complete`: the
    // (error) response was delivered in full via `on_header`, nothing is
    // truncated.
    if let Some(err) = check_ingress_cap(input.len()) {
        on_header(&err);
        return StreamOutcome::Complete;
    }
    let (header_bytes, body_bytes) = match split_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => {
            on_header(&error_wire(400, &msg));
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
    let (status, headers, metadata, body) = match dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        None,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        Some(&header_bytes),
        Body::from(body_bytes),
        default_json_when_absent,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => {
            on_header(&error_wire(status, &msg));
            return StreamOutcome::Complete;
        }
    };

    emit_header_then_stream_body(
        status,
        headers,
        metadata,
        body,
        &mut on_header,
        &mut on_chunk,
    )
    .await
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
/// a bounded mpsc channel ([`streaming_channel_capacity`] slots,
/// default 16): if axum reads slowly, the `pull_chunk` call blocks
/// naturally.
///
/// Failure modes match [`dispatch_streaming_async`]: malformed
/// header / unknown version / no app / handler error → normal
/// `error_wire(...)` response (with the message inside the returned
/// bytes); neither callback is invoked in those paths.
///
/// This is the ergonomic form with **no request-source close hook** —
/// the request producer is awaited to its natural completion.  Callers
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
        bidirectional_streaming_inner(input_header, pull_chunk, on_chunk, on_header, request_close)
            .await
    };
    match outcome {
        // `Complete` covers a clean drain AND the pre-dispatch error paths
        // (which deliver a full `error_wire(...)` via `on_header`), so the
        // captured bytes are authoritative.
        StreamOutcome::Complete => header_bytes,
        // The response body errored, or the chunk sink stopped, AFTER the
        // success header was captured into `header_bytes` — the delivered
        // body is truncated.  Replace the captured success header with a 500
        // so a truncated bidirectional response is never returned as a clean
        // success (mirrors `dispatch_streaming_async`).
        StreamOutcome::BodyError => error_wire(500, "response body stream error"),
        StreamOutcome::SinkStopped => {
            error_wire(500, "response body sink stopped before completion")
        }
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
    bidirectional_streaming_inner(input_header, pull_chunk, on_chunk, on_header, request_close)
        .await
}

async fn bidirectional_streaming_inner<P, F, H, C>(
    input_header: Vec<u8>,
    pull_chunk: P,
    mut on_chunk: F,
    mut on_header: H,
    request_close: C,
) -> StreamOutcome
where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]) -> ControlFlow<()>,
    H: FnMut(&[u8]),
    C: FnOnce(),
{
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

    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(None));
    let body = Body::new(ChannelBody::new(pull_chunk, Arc::clone(&producer_handle)));
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
    // dispatch_and_split detects Content-Type during its build pass, so we
    // pass `true` (default-when-absent) instead of running a separate
    // pre-scan: the streamed body's emptiness is unknowable up front, so we
    // default whenever no `Content-Type` header is present — byte-identical
    // to the prior `!has_content_type` semantics.
    let default_json_when_absent = true;
    // See the response-streaming sibling: streaming is body-throughput bound,
    // so pass `None` rather than threading the owned-path URI zero-copy here.
    // Share `header_bytes` for the per-request `HeaderValue` insertions so
    // each plain value is constructed zero-copy.
    let (status, headers, metadata, response_body) = match dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        None,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        Some(&header_bytes),
        body,
        default_json_when_absent,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => {
            // Pre-dispatch failure (bad method/path → 405/400): the producer
            // almost never started, but close defensively (no-op if it did
            // not) before awaiting so we cannot hang here either.
            closer.close_if_started();
            await_request_producer(&producer_handle).await;
            on_header(&error_wire(status, &msg));
            return StreamOutcome::Complete;
        }
    };

    let outcome = emit_header_then_stream_body(
        status,
        headers,
        metadata,
        response_body,
        &mut on_header,
        &mut on_chunk,
    )
    .await;

    // The response is fully drained, so the handler has finished and will
    // not read more of the request body. If the producer was started (the
    // handler read at least one chunk) it may be parked in a blocking source
    // read; close the request source to unblock it so the await below cannot
    // hang on a stuck / slow upload that never reaches EOF. A full read
    // already hit EOF (close is a no-op) and a producer that never started
    // leaves the source untouched. `close_if_started` is idempotent, so the
    // guard's Drop becomes a no-op on this happy path.
    closer.close_if_started();
    await_request_producer(&producer_handle).await;
    outcome
}

/// Lock the producer handle, transparently recovering the guard if the mutex
/// was poisoned.  A poison here only means a prior holder panicked while the
/// streaming path was tearing down; the guarded `Option<JoinHandle>` is still
/// structurally valid, so recovering and proceeding is correct — and keeps this
/// FFI-adjacent path free of the `unwrap` panic site each of the three call
/// sites would otherwise carry.  (Same idiom as the registry/bench read paths.)
fn lock_producer_handle(
    producer_handle: &RequestProducerHandle,
) -> std::sync::MutexGuard<'_, Option<tokio::task::JoinHandle<()>>> {
    producer_handle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether the request producer task was started — i.e. the handler read
/// at least one body chunk, which lazily spawns the producer.
fn producer_was_started(producer_handle: &RequestProducerHandle) -> bool {
    lock_producer_handle(producer_handle).is_some()
}

/// RAII guard that closes the request body source **exactly once** if the
/// request producer was started. [`bidirectional_streaming_inner`] uses it so
/// the close runs on every exit path, including a panic that unwinds out of
/// the handler or the response-body poll — the JNI boundary's `catch_unwind`
/// would otherwise turn the panic into a 500 and skip the explicit close,
/// leaking a producer parked in a blocking source read.
struct RequestSourceCloser<C: FnOnce()> {
    producer_handle: RequestProducerHandle,
    close: Option<C>,
}

impl<C: FnOnce()> RequestSourceCloser<C> {
    fn new(producer_handle: RequestProducerHandle, close: C) -> Self {
        Self {
            producer_handle,
            close: Some(close),
        }
    }

    /// Close the request source iff the producer was started. Idempotent: the
    /// close hook is consumed on the first call, so later calls (including the
    /// one in `Drop`) are no-ops. If the producer never started the hook is
    /// dropped uncalled — there is nothing to close.
    ///
    /// The hook runs under `catch_unwind`: `close_if_started` is also invoked
    /// from `Drop`, which can run while a panic is already unwinding out of the
    /// handler or the response-body poll, where a hook panic would be a
    /// double-panic → process `abort()` (taking the host JVM down with it). The
    /// close is best-effort cleanup (unblock a producer parked in a blocking
    /// read) that runs only AFTER the response is fully drained, so a panicking
    /// hook is contained rather than allowed to abort the process or fail an
    /// already-produced response.
    fn close_if_started(&mut self) {
        if let Some(close) = self.close.take()
            && producer_was_started(&self.producer_handle)
        {
            // `AssertUnwindSafe`: the hook is `FnOnce()` best-effort cleanup and
            // the producer is being torn down regardless, so swallowing its
            // panic leaves no observable state inconsistent.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(close));
        }
    }
}

impl<C: FnOnce()> Drop for RequestSourceCloser<C> {
    fn drop(&mut self) {
        // Runs on unwind when the happy-path `close_if_started()` did not.
        self.close_if_started();
    }
}

type RequestProducerHandle = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;
type PullChunk = Box<dyn FnMut() -> RequestChunk + Send + 'static>;
type RequestFrame = Result<Bytes, StreamAbort>;

struct RequestProducer {
    pull_chunk: PullChunk,
    capacity: usize,
}

/// Minimal `http_body::Body` implementation backed by an mpsc
/// `Receiver<Result<Bytes, StreamAbort>>` — used by
/// [`dispatch_bidirectional_streaming`] to feed request body chunks
/// into axum.  A producer error is forwarded as a body error so a
/// truncated upload is not seen as a clean EOF.
struct ChannelBody {
    rx: Option<tokio::sync::mpsc::Receiver<RequestFrame>>,
    producer: Option<RequestProducer>,
    producer_handle: RequestProducerHandle,
}

impl ChannelBody {
    fn new<P>(pull_chunk: P, producer_handle: RequestProducerHandle) -> Self
    where
        P: FnMut() -> RequestChunk + Send + 'static,
    {
        Self {
            rx: None,
            producer: Some(RequestProducer {
                pull_chunk: Box::new(pull_chunk),
                // Product-capped (chunk_bytes * slots <= 64 MiB) so a large
                // configured chunk size can't multiply with the channel
                // capacity into multi-GB peak buffering. See
                // `effective_streaming_channel_capacity`.
                capacity: effective_streaming_channel_capacity(),
            }),
            producer_handle,
        }
    }

    fn start_producer_if_needed(&mut self) {
        if self.rx.is_some() {
            return;
        }

        let Some(producer) = self.producer.take() else {
            return;
        };

        // Bounded mpsc (default 16 slots, see streaming_channel_capacity)
        // — gives natural backpressure between the pull_chunk producer
        // thread and the axum handler consumer. The channel is created
        // with the producer so unpolled bodies avoid both pieces of setup.
        let (tx, rx) = tokio::sync::mpsc::channel::<RequestFrame>(producer.capacity);
        self.rx = Some(rx);
        let handle = spawn_request_producer(producer.pull_chunk, tx);
        store_request_producer_handle(&self.producer_handle, handle);
    }
}

impl HttpBody for ChannelBody {
    type Data = Bytes;
    type Error = StreamAbort;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.start_producer_if_needed();

        let Some(rx) = self.rx.as_mut() else {
            return Poll::Ready(None);
        };

        match rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            // Producer reported an abort: surface it as a body error so
            // axum/the handler rejects the truncated upload.
            Poll::Ready(Some(Err(abort))) => Poll::Ready(Some(Err(abort))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn spawn_request_producer(
    mut pull: PullChunk,
    tx: tokio::sync::mpsc::Sender<RequestFrame>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        // `End` ends the stream; an empty `Data(_)` is skipped (it's not
        // EOF); `Error` forwards a `StreamAbort` so the body errors out
        // instead of ending cleanly.  A failed `blocking_send` means the
        // receiver — axum's request body — was dropped because the
        // handler aborted mid-stream, so we stop pulling.
        let mut consecutive_empty: u32 = 0;
        // Read once: the configured max bytes per queued frame. A host
        // `pull()` may return an arbitrarily large `Vec`; splitting it into
        // `<= max_chunk` pieces below keeps the channel's `slots * chunk_bytes`
        // memory bound REAL instead of `slots * arbitrary` — without it a
        // hostile/buggy producer returning multi-MiB chunks defeats the
        // `O(chunk)` RAM guarantee and can OOM the host under load.
        let max_chunk = crate::config::streaming_chunk_bytes();
        loop {
            // A panic inside the user / JNI-supplied `pull()` must NOT be
            // turned into a clean end-of-stream — that would accept a
            // TRUNCATED upload as a complete request body (silent data
            // loss).  Catch it and forward a `StreamAbort`, exactly like the
            // explicit `RequestChunk::Error` path, so axum/the handler
            // rejects the body instead of seeing a short, "successful" read.
            let Ok(next) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut pull)) else {
                let _ = tx.blocking_send(Err(StreamAbort));
                break;
            };
            match next {
                RequestChunk::Data(chunk) => {
                    if chunk.is_empty() {
                        // A conformant blocking `InputStream.read(byte[])`
                        // never returns 0 for a non-empty buffer — it
                        // blocks until ≥1 byte or returns -1 at EOF.
                        // Sustained empty reads therefore mean a stuck or
                        // hostile producer; cap them (with a yield so we
                        // don't peg a blocking-pool core) and abort instead
                        // of busy-spinning this thread forever.
                        consecutive_empty += 1;
                        if consecutive_empty >= MAX_CONSECUTIVE_EMPTY_READS {
                            let _ = tx.blocking_send(Err(StreamAbort));
                            break;
                        }
                        std::thread::yield_now();
                        continue;
                    }
                    consecutive_empty = 0;
                    // Enforce the per-frame size cap: split an oversized host
                    // chunk into `<= max_chunk` pieces so each QUEUED frame is
                    // bounded and the channel's slot accounting reflects real
                    // bytes (a 100 MiB host chunk no longer occupies a slot as
                    // 100 MiB).  `Bytes::split_to` is an O(1) refcount slice —
                    // no copy — and a conformant `<= max_chunk` chunk (the JNI
                    // reader always reads into a `chunk_bytes` buffer, and the
                    // benches pre-chunk at `chunk_bytes`) sends in a single
                    // iteration exactly as before.
                    let mut bytes = Bytes::from(chunk);
                    let mut receiver_gone = false;
                    while !bytes.is_empty() {
                        let piece = if bytes.len() > max_chunk {
                            bytes.split_to(max_chunk)
                        } else {
                            std::mem::take(&mut bytes)
                        };
                        if tx.blocking_send(Ok(piece)).is_err() {
                            receiver_gone = true;
                            break;
                        }
                    }
                    if receiver_gone {
                        break;
                    }
                }
                RequestChunk::End => break,
                RequestChunk::Error => {
                    // Best-effort: if the receiver is already gone there
                    // is nothing to abort.
                    let _ = tx.blocking_send(Err(StreamAbort));
                    break;
                }
            }
        }
        // tx dropped at end of scope → axum sees end-of-stream (or the
        // forwarded error above).
    })
}

fn store_request_producer_handle(
    producer_handle: &RequestProducerHandle,
    handle: tokio::task::JoinHandle<()>,
) {
    *lock_producer_handle(producer_handle) = Some(handle);
}

async fn await_request_producer(producer_handle: &RequestProducerHandle) {
    // Take the handle and release the guard on the same statement: a
    // `MutexGuard` is not `Send` and must never be held across the `.await`.
    let handle = lock_producer_handle(producer_handle).take();
    if let Some(handle) = handle {
        let _ = handle.await;
    }
}

#[cfg(test)]
mod tests;
