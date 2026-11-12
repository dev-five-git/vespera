//! Request-body producer for the bidirectional streaming paths: the
//! channel-backed request [`Body`](axum::body::Body), the lazily spawned
//! `spawn_blocking` producer that pulls host chunks into it, the RAII
//! request-source closer, and the bounded producer join.
//!
//! Split out of `streaming.rs` to keep that file within the 1000-line cap.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};

use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};

use super::{MAX_CONSECUTIVE_EMPTY_READS, RequestChunk, StreamAbort};
use crate::config::{effective_streaming_channel_capacity, max_streaming_request_producers};

/// Process-wide request-producer slots ([`max_streaming_request_producers`]);
/// `None` when uncapped.  Built on the first producer start, after which the
/// cap is fixed like every other knob.
static PRODUCER_SLOTS: OnceLock<Option<Arc<Semaphore>>> = OnceLock::new();

fn producer_slots() -> Option<&'static Arc<Semaphore>> {
    PRODUCER_SLOTS
        .get_or_init(|| match max_streaming_request_producers() {
            0 => None,
            max => Some(Arc::new(Semaphore::new(max.min(Semaphore::MAX_PERMITS)))),
        })
        .as_ref()
}

type PermitFuture =
    Pin<Box<dyn Future<Output = Result<OwnedSemaphorePermit, AcquireError>> + Send>>;

/// How long a finished bidirectional dispatch waits for the request
/// producer to exit after the request source was closed.
///
/// The join exists so the host can safely reuse (or recycle) the request
/// source and its chunk buffer once the dispatch returns: a producer that
/// is still inside `InputStream.read` would otherwise race the next
/// request on the same pooled stream / buffer.  The close hook normally
/// unblocks a parked read immediately, but a host stream whose `close()`
/// does NOT interrupt a read blocked on another thread would previously
/// hang the dispatch (and the Java caller) forever.  After this grace the
/// producer is **detached**: it keeps its own `Arc`s (pull closure, source
/// and buffer refs) alive until it exits on its own, the response is
/// returned, and the JNI layer — which observes whether the pull closure
/// was released — discards (rather than reuses) the still-leased chunk
/// buffer.  Requires the Tokio time driver only on this slow path (a
/// producer that already finished is joined without a timer).
pub const REQUEST_PRODUCER_JOIN_GRACE: Duration = Duration::from_secs(2);

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
/// request producer was started. `bidirectional_streaming_inner` uses it so
/// the close runs on every exit path, including a panic that unwinds out of
/// the handler or the response-body poll — the JNI boundary's `catch_unwind`
/// would otherwise turn the panic into a 500 and skip the explicit close,
/// leaking a producer parked in a blocking source read.
pub struct RequestSourceCloser<C: FnOnce()> {
    producer_handle: RequestProducerHandle,
    close: Option<C>,
}

impl<C: FnOnce()> RequestSourceCloser<C> {
    pub fn new(producer_handle: RequestProducerHandle, close: C) -> Self {
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
    pub fn close_if_started(&mut self) {
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

pub type RequestProducerHandle = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;
type PullChunk = Box<dyn FnMut() -> RequestChunk + Send + 'static>;
type RequestFrame = Result<Bytes, StreamAbort>;

/// Minimal `http_body::Body` implementation backed by an mpsc
/// `Receiver<Result<Bytes, StreamAbort>>` — used by
/// `dispatch_bidirectional_streaming` to feed request body chunks
/// into axum.  A producer error is forwarded as a body error so a
/// truncated upload is not seen as a clean EOF.
///
/// Also enforces [`crate::max_streaming_request_bytes`]: once the received
/// total exceeds `max_bytes` (`0` = unlimited) the body errors out with
/// [`StreamAbort`], the channel is dropped (stopping the producer), and
/// `limit_exceeded` is latched so the dispatcher can answer `413` if the
/// response header has not been committed yet.
pub struct ChannelBody {
    pub(super) rx: Option<tokio::sync::mpsc::Receiver<RequestFrame>>,
    /// One-shot producer slot: `Some` until the first `poll_frame` lazily
    /// spawns the producer task, then permanently `None`. `Option::take`
    /// on this field is the same atomic transition that the prior
    /// `Option<RequestProducer>` provided, so the lazy-start semantics
    /// (channel + producer NOT created until the handler polls the body)
    /// are preserved.
    pub(super) pull_chunk: Option<PullChunk>,
    /// Product-capped channel capacity (chunk_bytes * slots <= 64 MiB)
    /// so a large configured chunk size can't multiply with the channel
    /// capacity into multi-GB peak buffering. See
    /// [`effective_streaming_channel_capacity`].
    pub(super) capacity: usize,
    pub(super) producer_handle: RequestProducerHandle,
    /// Request-body byte cap (`0` = unlimited).
    pub(super) max_bytes: usize,
    /// Body bytes delivered to the handler so far.
    pub(super) received: usize,
    /// Latched when `received` exceeded `max_bytes`.
    pub(super) limit_exceeded: Arc<AtomicBool>,
    /// Set by the producer ONLY when the host reported a clean
    /// [`RequestChunk::End`].  A channel that closes without it (the
    /// `spawn_blocking` task was dropped unrun at runtime shutdown, or the
    /// producer exited any other way) is an aborted upload, never a clean
    /// EOF — see the `Poll::Ready(None)` arm of `poll_frame`.
    pub(super) clean_end: Arc<AtomicBool>,
    /// In-progress wait for a producer slot while the cap is saturated.
    pub(super) permit_wait: Option<PermitFuture>,
}

impl ChannelBody {
    pub fn new<P>(
        pull_chunk: P,
        producer_handle: RequestProducerHandle,
        max_bytes: usize,
        limit_exceeded: Arc<AtomicBool>,
    ) -> Self
    where
        P: FnMut() -> RequestChunk + Send + 'static,
    {
        Self {
            rx: None,
            pull_chunk: Some(Box::new(pull_chunk)),
            capacity: effective_streaming_channel_capacity(),
            producer_handle,
            max_bytes,
            received: 0,
            limit_exceeded,
            clean_end: Arc::new(AtomicBool::new(false)),
            permit_wait: None,
        }
    }

    /// Start the producer on the first poll, once a producer slot
    /// ([`max_streaming_request_producers`]) is available.  `Pending` while
    /// waiting for a slot (no thread is held meanwhile).
    pub(super) fn poll_start_producer(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.rx.is_some() || self.pull_chunk.is_none() {
            return Poll::Ready(());
        }
        let permit = match producer_slots() {
            None => None,
            Some(slots) => match self.poll_producer_slot(slots, cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(permit) => permit,
            },
        };
        self.start_producer(permit);
        Poll::Ready(())
    }

    /// Acquire one producer slot: an uncontended `try_acquire_owned` costs
    /// no allocation; only a saturated cap boxes the waiting future.
    pub(super) fn poll_producer_slot(
        &mut self,
        slots: &Arc<Semaphore>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<OwnedSemaphorePermit>> {
        if self.permit_wait.is_none()
            && let Ok(permit) = Arc::clone(slots).try_acquire_owned()
        {
            return Poll::Ready(Some(permit));
        }
        let wait = self
            .permit_wait
            .get_or_insert_with(|| Box::pin(Arc::clone(slots).acquire_owned()));
        let acquired = std::task::ready!(wait.as_mut().poll(cx));
        self.permit_wait = None;
        // The semaphore is never closed; an `Err` ("closed") would only run
        // this producer uncapped rather than stall the upload.
        Poll::Ready(acquired.ok())
    }

    fn start_producer(&mut self, permit: Option<OwnedSemaphorePermit>) {
        let Some(pull) = self.pull_chunk.take() else {
            return;
        };

        // Bounded mpsc (default 16 slots, see effective_streaming_channel_capacity)
        // — gives natural backpressure between the pull_chunk producer
        // thread and the axum handler consumer. The channel is created
        // with the producer so unpolled bodies avoid both pieces of setup.
        let (tx, rx) = tokio::sync::mpsc::channel::<RequestFrame>(self.capacity);
        self.rx = Some(rx);
        let handle = spawn_request_producer(pull, tx, Arc::clone(&self.clean_end), permit);
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
        if self.poll_start_producer(cx).is_pending() {
            return Poll::Pending;
        }

        let Some(rx) = self.rx.as_mut() else {
            return Poll::Ready(None);
        };

        match rx.poll_recv(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                self.received = self.received.saturating_add(bytes.len());
                if self.max_bytes != 0 && self.received > self.max_bytes {
                    // Over the streaming request cap: latch the flag, drop the
                    // receiver (the producer's next send fails → it stops
                    // pulling) and fail the body instead of truncating it.
                    self.limit_exceeded.store(true, Ordering::Release);
                    self.rx = None;
                    return Poll::Ready(Some(Err(StreamAbort)));
                }
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            // Producer reported an abort: surface it as a body error so
            // axum/the handler rejects the truncated upload.
            Poll::Ready(Some(Err(abort))) => {
                self.rx = None;
                Poll::Ready(Some(Err(abort)))
            }
            // The channel closed.  Only a producer that saw the host's
            // explicit `End` marks the body complete; any other close (the
            // `spawn_blocking` task dropped unrun during runtime shutdown, a
            // producer torn down mid-stream) must not be mistaken for a clean
            // EOF — that would accept a truncated upload as complete.
            Poll::Ready(None) => {
                self.rx = None;
                if self.clean_end.load(Ordering::Acquire) {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Err(StreamAbort)))
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

fn spawn_request_producer(
    mut pull: PullChunk,
    tx: tokio::sync::mpsc::Sender<RequestFrame>,
    clean_end: Arc<AtomicBool>,
    permit: Option<OwnedSemaphorePermit>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        // Producer slot: released when this blocking task ends — or when the
        // runtime drops it unrun at shutdown.
        let _permit = permit;
        // Fallback sender for the OUTER panic guard: if any panic escapes
        // the producer loop below (not just `pull()`, but `Bytes::from`, the
        // chunk-splitter `while`, or `tx.blocking_send`), the catch below
        // surfaces it as `StreamAbort` so axum sees an errored body — the
        // same "no silent truncation" discipline the prior per-pull catch
        // enforced, now extended to the whole loop body.  `mpsc::Sender`'s
        // `Clone` is a single `Arc` bump paid ONCE per producer spawn (not
        // per chunk); the outer `tx` still moves into the closure below so
        // the happy path uses exactly one sender.
        let tx_fallback = tx.clone();
        // ONE outer `catch_unwind` replaces the previous per-pull catch.
        // Two independent wins over the old per-pull shape:
        //   1. Cost — installing a landing-pad frame ONCE per producer spawn
        //      instead of once per chunk (~4K setups per 1 GiB / 256 KiB
        //      stream) removes an unnecessary per-chunk cost on the happy
        //      path.  Every code path already handled a caught panic
        //      identically to `RequestChunk::Error`, so an outer catch
        //      preserves the observable outcome for a panicking `pull()`.
        //   2. Semantics — a panic ANYWHERE ELSE in the loop body (`Bytes::
        //      from(chunk)`, the chunk-splitter `while`, `tx.blocking_send`,
        //      `consecutive_empty.saturating_add`) used to unwind past the
        //      per-pull catch, abort the `spawn_blocking` task, and surface
        //      as a `JoinError` that `await_request_producer` silently
        //      discards (`let _ = handle.await;`) — axum then saw a CLEAN
        //      end-of-stream instead of a `StreamAbort`, i.e. a TRUNCATED
        //      upload accepted as a complete request body (silent data
        //      loss, the exact failure the per-pull catch was written to
        //      prevent).  The outer catch closes that hole so ANY loop-body
        //      panic now surfaces as `StreamAbort`.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
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
            // `'producer:` lets every early-exit path (sustained empty reads,
            // explicit `End`/`Error`, receiver-side channel drop in the inner
            // chunk-splitter `while`) exit the same `loop` directly without a
            // `receiver_gone` flag + post-`while` check.  A panic anywhere
            // in this body no longer breaks out of the loop — it unwinds
            // straight to the outer `catch_unwind`'s `Err` arm above, which
            // sends the same `StreamAbort` on `tx_fallback`.
            'producer: loop {
                let next = pull();
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
                            //
                            // `saturating_add` aligns this producer-side counter
                            // with its sibling `consecutive_empty_reads` in
                            // `crates/vespera_jni/src/streaming_closures.rs` —
                            // a directly comparable layered-defence counter with
                            // the same panic-free discipline documented at
                            // `streaming_closures.rs:141-146`.  Release codegen
                            // is byte-identical to `+= 1` (LLVM lowers both to
                            // `add.i32`); the change removes a theoretical
                            // debug-only overflow panic that MAX_CONSECUTIVE_
                            // EMPTY_READS = 1024 already prevents in practice.
                            consecutive_empty = consecutive_empty.saturating_add(1);
                            if consecutive_empty >= MAX_CONSECUTIVE_EMPTY_READS {
                                let _ = tx.blocking_send(Err(StreamAbort));
                                break 'producer;
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
                        while !bytes.is_empty() {
                            let piece = if bytes.len() > max_chunk {
                                bytes.split_to(max_chunk)
                            } else {
                                std::mem::take(&mut bytes)
                            };
                            // Receiver gone → axum dropped the body half (the
                            // dispatch finished or the handler abandoned the
                            // request).  Stop the whole producer, identical
                            // outcome to the prior `receiver_gone` flag.
                            if tx.blocking_send(Ok(piece)).is_err() {
                                break 'producer;
                            }
                        }
                    }
                    RequestChunk::End => {
                        // Published BEFORE `tx` drops, so the receiver sees
                        // the flag once it observes the closed channel.
                        clean_end.store(true, Ordering::Release);
                        break 'producer;
                    }
                    RequestChunk::Error => {
                        // Best-effort: if the receiver is already gone there
                        // is nothing to abort.
                        let _ = tx.blocking_send(Err(StreamAbort));
                        break 'producer;
                    }
                }
            }
        }));
        // Any panic that escaped the loop above surfaces here as a
        // `StreamAbort` on the pre-cloned fallback sender — best-effort,
        // a no-op if the receiver is already gone.
        if outcome.is_err() {
            let _ = tx_fallback.blocking_send(Err(StreamAbort));
        }
        // tx / tx_fallback dropped at end of scope → axum sees end-of-stream
        // (or the forwarded error above).
    })
}

fn store_request_producer_handle(
    producer_handle: &RequestProducerHandle,
    handle: tokio::task::JoinHandle<()>,
) {
    *lock_producer_handle(producer_handle) = Some(handle);
}

/// Join the request producer (if it was started), waiting at most
/// [`REQUEST_PRODUCER_JOIN_GRACE`]; a producer still running after the
/// grace is detached (see the constant's docs).  Called only AFTER the
/// request source was closed, so the wait is normally instantaneous.
pub async fn join_request_producer(producer_handle: &RequestProducerHandle) {
    // Take the handle and release the guard on the same statement: a
    // `MutexGuard` is not `Send` and must never be held across the `.await`.
    let handle = lock_producer_handle(producer_handle).take();
    let Some(handle) = handle else {
        return;
    };
    // Fast path: an already-finished producer is joined without arming a
    // timer (so the common case needs no Tokio time driver).
    if handle.is_finished() {
        let _ = handle.await;
        return;
    }
    // Slow path: bounded wait. On elapse the `JoinHandle` is dropped, which
    // DETACHES (does not cancel) the blocking task — it owns everything it
    // touches and exits on its own once its read returns.
    let _ = tokio::time::timeout(REQUEST_PRODUCER_JOIN_GRACE, handle).await;
}
