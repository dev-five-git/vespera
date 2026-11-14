//! Inline request-body pulls: the small-body fast path of the
//! bidirectional streaming request [`ChannelBody`].
//!
//! Starting the request producer costs a producer slot, a bounded `mpsc`
//! channel, a `spawn_blocking` hand-off to another thread (where the JNI
//! pull closure must also resolve its `JNIEnv`) and a cross-thread wake
//! per chunk.  For a small body — one chunk followed by `End` — that fixed
//! setup dominates the whole dispatch.
//!
//! When the dispatch future is driven **directly by `Runtime::block_on`**
//! (no Tokio task id — the JNI symbols and other blocking FFI callers), the
//! calling thread is already a thread that may block, so the first
//! [`INLINE_PULL_BUDGET`] pulls run right there inside `poll_frame`.  A
//! body that ends within the budget never starts the producer at all; a
//! longer one falls back to the spawned producer for the rest, which keeps
//! pulling from the same closure (no chunk is lost or reordered).
//!
//! Inline pulls are only taken while they cannot change observable
//! behaviour beyond latency:
//! - **before the handler returned its response** ([`InlineState`], cleared
//!   by the dispatcher): a body polled while the response streams may be
//!   an interactive protocol whose client waits for response bytes (and the
//!   "flush before waiting" hint) before sending more — such a read must
//!   never block the thread that delivers the response;
//! - **without a [`crate::dispatch_timeout`]**: a blocking inline read
//!   cannot be pre-empted by the deadline, whereas a parked producer is
//!   unblocked by the request-source close that follows a `504`;
//! - **outside a spawned task**: a pull must never block a runtime worker.
//!
//! **Caveat — handler-side timeouts.** An inline pull blocks the polling
//! thread *inside* `poll_frame`, so a timeout the handler itself puts around
//! a body read (`tokio::time::timeout(d, body.frame())`, a `select!` against
//! a `sleep`) cannot fire while that read blocks: the timer is only looked at
//! after the poll returns, i.e. once the host source delivered a chunk, `End`
//! or an error.  This affects at most the first [`INLINE_PULL_BUDGET`] reads
//! of a request.  A handler that must bound those reads with its own timer
//! should either read the body inside a spawned task (a task id disables the
//! fast path, the producer then blocks a `spawn_blocking` thread instead) or
//! rely on [`crate::dispatch_timeout`], which disables the fast path for the
//! whole process.
//!
//! Backpressure is trivially preserved (a pull happens only when the
//! handler asks for a frame), the [`crate::max_streaming_request_bytes`]
//! cap is enforced on the same running total, and a pull that reports
//! [`RequestChunk::Error`] — or panics — fails the body with
//! [`StreamAbort`] exactly like the producer would.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use http_body::Frame;

use super::producer::ChannelBody;
use super::{RequestChunk, StreamAbort};

/// Inline pulls allowed per request body: the first chunk plus the probe
/// that observes `End` for a one-chunk body.  Further pulls go through the
/// spawned producer.
pub(super) const INLINE_PULL_BUDGET: u8 = 2;

/// One inline poll result: `None` = fall back to the producer.
type InlineFrame = Option<Option<Result<Frame<Bytes>, StreamAbort>>>;

impl ChannelBody {
    /// Whether the next frame may be pulled on the polling thread.
    fn inline_pull_allowed(&self) -> bool {
        self.inline_budget > 0
            && self.rx.is_none()
            && self.pull_chunk.is_some()
            && self.inline.open.load(Ordering::Acquire)
            && tokio::task::try_id().is_none()
    }

    /// Pull the next frame inline when allowed (see the module docs).
    /// Returns `None` when the producer must take over; otherwise the
    /// frame for `poll_frame` to return.
    pub(super) fn poll_inline(&mut self) -> InlineFrame {
        while self.inline_pull_allowed() {
            self.inline_budget -= 1;
            self.inline.touched.store(true, Ordering::Release);
            let pull = self.pull_chunk.as_mut()?;
            // Same discipline as the producer's outer `catch_unwind`: a
            // panicking pull aborts the body instead of unwinding into the
            // handler (or across the FFI boundary of the caller).
            let chunk = std::panic::catch_unwind(std::panic::AssertUnwindSafe(pull))
                .unwrap_or(RequestChunk::Error);
            match chunk {
                // "Nothing right now, keep open": spend budget, retry.
                RequestChunk::Data(data) if data.is_empty() => {}
                RequestChunk::Data(data) => return Some(Some(self.inline_data(data))),
                RequestChunk::End => {
                    self.pull_chunk = None;
                    return Some(None);
                }
                RequestChunk::Error => {
                    self.pull_chunk = None;
                    return Some(Some(Err(StreamAbort)));
                }
            }
        }
        None
    }

    /// Account an inline chunk against the streaming request cap.
    fn inline_data(&mut self, data: Vec<u8>) -> Result<Frame<Bytes>, StreamAbort> {
        self.received = self.received.saturating_add(data.len());
        if self.max_bytes != 0 && self.received > self.max_bytes {
            // Over the cap: latch the flag and drop the source closure
            // (nothing more is pulled), failing the body like the
            // channel path does.
            self.limit_exceeded.store(true, Ordering::Release);
            self.pull_chunk = None;
            return Err(StreamAbort);
        }
        Ok(Frame::data(Bytes::from(data)))
    }
}

/// Inline-pull state shared between a request [`ChannelBody`], the
/// dispatcher and the request-source closer.
#[derive(Debug)]
pub(super) struct InlineState {
    /// Inline pulls are allowed only while set: cleared once the handler
    /// returned its response, never set when a dispatch timeout is armed.
    open: AtomicBool,
    /// The request source was pulled inline at least once.
    touched: AtomicBool,
    /// The dispatch finished (set by the request-source closer under the
    /// producer-handle lock): a body that outlived it never reads again.
    finished: AtomicBool,
}

impl InlineState {
    pub(super) fn new(open: bool) -> Arc<Self> {
        Arc::new(Self {
            open: AtomicBool::new(open),
            touched: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        })
    }

    /// The dispatch returned (or is unwinding): no further source reads,
    /// inline or through a producer.
    pub(super) fn finish(&self) {
        self.open.store(false, Ordering::Release);
        self.finished.store(true, Ordering::Release);
    }

    /// Whether [`InlineState::finish`] ran.
    pub(super) fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// The handler returned: no further inline pulls.
    pub(super) fn close(&self) {
        self.open.store(false, Ordering::Release);
    }

    /// Whether the request source was pulled inline — the closer then
    /// closes it after the response like a started producer's source.
    pub(super) fn touched(&self) -> bool {
        self.touched.load(Ordering::Acquire)
    }
}
