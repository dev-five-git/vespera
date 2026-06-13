//! Streaming dispatch variants: response streaming, header-callback
//! streaming, and bidirectional (request + response) streaming.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use axum::body::Body;
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use http_body_util::BodyExt;

use crate::config::streaming_channel_capacity;
use crate::internal::{dispatch_and_split, dispatch_response_streaming};
use crate::registry::resolve_app_router;
use crate::wire::{
    WIRE_HEADER_RESERVE, WIRE_VERSION, build_wire_header_bytes, error_wire, parse_wire_header,
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
    F: FnMut(&[u8]),
{
    let (header_bytes, body_bytes) = match split_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => return error_wire(400, &msg),
    };
    let header = match parse_wire_header(&header_bytes) {
        Ok(h) => h,
        Err(msg) => return error_wire(400, &msg),
    };
    if header.v != WIRE_VERSION {
        return error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        );
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => return wire,
    };
    let (status, headers, metadata) = match dispatch_response_streaming(
        router,
        &header.method,
        &header.path,
        &header.query,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        body_bytes,
        &mut on_chunk,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => return error_wire(status, &msg),
    };
    // Emit header-only wire bytes; body was streamed via on_chunk.
    // NOTE: the streaming path does not hoist 422 validation errors —
    // hoisting requires materialising the full body, which is
    // antithetical to the streaming contract.  Callers needing
    // validation hoisting should use dispatch_from_bytes_async.
    build_wire_header_bytes(status, &headers, &metadata)
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
) where
    H: FnMut(&[u8]),
    F: FnMut(&[u8]),
{
    let (header_bytes, body_bytes) = match split_wire_request(input) {
        Ok(parts) => parts,
        Err(msg) => {
            on_header(&error_wire(400, &msg));
            return;
        }
    };
    let header = match parse_wire_header(&header_bytes) {
        Ok(h) => h,
        Err(msg) => {
            on_header(&error_wire(400, &msg));
            return;
        }
    };
    if header.v != WIRE_VERSION {
        on_header(&error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        ));
        return;
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => {
            on_header(&wire);
            return;
        }
    };

    let (status, headers, metadata, mut body) = match dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        Body::from(body_bytes),
        false,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => {
            on_header(&error_wire(status, &msg));
            return;
        }
    };

    on_header(&build_wire_header_bytes(status, &headers, &metadata));

    while let Some(Ok(frame)) = body.frame().await {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
        {
            on_chunk(data.as_ref());
        }
    }
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
pub async fn dispatch_bidirectional_streaming<P, F>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
) -> Vec<u8>
where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]),
{
    let mut header_bytes: Vec<u8> = Vec::with_capacity(4 + WIRE_HEADER_RESERVE);
    {
        let on_header = |h: &[u8]| header_bytes.extend_from_slice(h);
        bidirectional_streaming_inner(input_header, pull_chunk, on_chunk, on_header).await;
    }
    header_bytes
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
pub async fn dispatch_bidirectional_streaming_with_header<P, F, H>(
    input_header: Vec<u8>,
    pull_chunk: P,
    on_chunk: F,
    on_header: H,
) where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]),
    H: FnMut(&[u8]),
{
    bidirectional_streaming_inner(input_header, pull_chunk, on_chunk, on_header).await;
}

async fn bidirectional_streaming_inner<P, F, H>(
    input_header: Vec<u8>,
    pull_chunk: P,
    mut on_chunk: F,
    mut on_header: H,
) where
    P: FnMut() -> RequestChunk + Send + 'static,
    F: FnMut(&[u8]),
    H: FnMut(&[u8]),
{
    let (header_bytes, _ignored_body) = match split_wire_request(input_header) {
        Ok(parts) => parts,
        Err(msg) => {
            on_header(&error_wire(400, &msg));
            return;
        }
    };
    let header = match parse_wire_header(&header_bytes) {
        Ok(h) => h,
        Err(msg) => {
            on_header(&error_wire(400, &msg));
            return;
        }
    };
    if header.v != WIRE_VERSION {
        on_header(&error_wire(
            400,
            &format!(
                "unsupported wire version: got {}, expected {WIRE_VERSION}",
                header.v
            ),
        ));
        return;
    }
    let router = match resolve_app_router(&header) {
        Ok(r) => r,
        Err(wire) => {
            on_header(&wire);
            return;
        }
    };

    let producer_handle: RequestProducerHandle = Arc::new(Mutex::new(None));
    let body = Body::new(ChannelBody::new(pull_chunk, Arc::clone(&producer_handle)));
    let (status, headers, metadata, mut response_body) = match dispatch_and_split(
        router,
        &header.method,
        &header.path,
        &header.query,
        header.headers.iter().map(|(k, v)| (k.as_ref(), v.as_ref())),
        body,
        false,
    )
    .await
    {
        Ok(parts) => parts,
        Err((status, msg)) => {
            await_request_producer(&producer_handle).await;
            on_header(&error_wire(status, &msg));
            return;
        }
    };

    on_header(&build_wire_header_bytes(status, &headers, &metadata));

    while let Some(Ok(frame)) = response_body.frame().await {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
        {
            on_chunk(data.as_ref());
        }
    }

    await_request_producer(&producer_handle).await;
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
                capacity: streaming_channel_capacity(),
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
        loop {
            match pull() {
                RequestChunk::Data(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    if tx.blocking_send(Ok(Bytes::from(chunk))).is_err() {
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
    match producer_handle.lock() {
        Ok(mut guard) => *guard = Some(handle),
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            *guard = Some(handle);
        }
    }
}

async fn await_request_producer(producer_handle: &RequestProducerHandle) {
    let handle = match producer_handle.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            guard.take()
        }
    };

    if let Some(handle) = handle {
        let _ = handle.await;
    }
}
