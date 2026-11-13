//! Ready-frame coalescing for the header-first streaming body tail.

use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::task::Poll;

use axum::body::Body;
use bytes::Bytes;
use http_body::Body as _;

/// Poll `body` for its next frame exactly once, without waiting: `Pending`
/// means the body has nothing ready right now (its waker is registered, so a
/// following `body.frame().await` resumes normally).
pub(super) fn poll_frame_now(
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
pub(super) struct FrameBatch {
    held: Option<Bytes>,
    merged: Vec<u8>,
    cap: usize,
}

impl FrameBatch {
    pub(super) const fn new(cap: usize) -> Self {
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
    pub(super) fn push<F>(&mut self, data: Bytes, on_chunk: &mut F) -> ControlFlow<()>
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
    pub(super) fn deliver<F>(&mut self, on_chunk: &mut F) -> ControlFlow<()>
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
