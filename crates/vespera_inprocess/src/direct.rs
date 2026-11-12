//! Direct-write tail shared by the `dispatch_into*` entry points: write the
//! wire response straight into a caller-owned buffer, and — on the
//! **resumable** path — hand an overflowing response back as a
//! [`PendingDirectResponse`] instead of discarding it.
//!
//! Split out of `dispatch.rs` to keep that file within the 1000-line cap.

use std::ops::ControlFlow;

use axum::body::Body;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_body_util::BodyExt;

use crate::bounded::{
    Deadline, Prefetched422, prefetch_422_body, response_too_large_wire, timeout_wire,
};
use crate::config::max_buffered_response_bytes;
use crate::envelope::ResponseMetadata;
use crate::internal::BODY_STREAM_ERROR_MSG;
use crate::streaming::{StreamOutcome, stream_body_tail};
use crate::wire::{
    build_wire_header_bytes, error_wire, to_wire_bytes, write_wire_header_into_slice,
};

/// Outcome of [`crate::dispatch_into_async`] / [`crate::dispatch_into`].
///
/// Carries the truncation/overflow discriminant the FFI bridge relies on
/// to decide whether `out[..n]` is a complete wire response or the
/// response needs a larger buffer.  Dropping this value silently treats
/// `Overflow(required)` as `Complete(n)`, exposing the
/// read-uninitialised-prefix hazard documented on
/// [`DirectWriteResult::Overflow`] — hence `#[must_use]`.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectWriteResult {
    /// A complete wire response occupies `out[0..n]`.
    Complete(usize),
    /// The response needs `required` bytes and `out` was too small.
    /// `out` contents are **undefined** (a prefix may have been
    /// written).  `required` is exact — a retry with a buffer of at
    /// least this size succeeds, but **re-runs the handler**.  Use
    /// [`crate::dispatch_into_async_borrowed_resumable`] to keep the
    /// response instead.
    Overflow(usize),
}

/// Outcome of [`crate::dispatch_into_async_borrowed_resumable`] and
/// [`PendingDirectResponse::write_into`].
#[must_use]
#[derive(Debug)]
pub enum ResumableDirectWrite {
    /// A complete wire response occupies `out[0..n]`.
    Complete(usize),
    /// The response did not fit `out` (whose contents are undefined).  The
    /// response is **kept**: continue it with
    /// [`PendingDirectResponse::write_into`] (a larger buffer) or
    /// [`PendingDirectResponse::stream`] — the handler is never re-run.
    Overflow(PendingDirectResponse),
}

/// A direct-write response that overflowed the caller's buffer, kept so it
/// can be delivered without re-running the handler.
///
/// Holds the serialized wire header, the body bytes already produced, and
/// the **unread** remainder of the response body: nothing past the
/// caller's buffer was drained, so an unbounded or slow body (a long
/// download, an event stream) costs no memory or time here and can be
/// handed to a streaming sink instead.  Dropping it drops the response.
pub struct PendingDirectResponse {
    header: Vec<u8>,
    prefix: Vec<u8>,
    body: Option<Body>,
    exact_remaining: Option<u64>,
    prefers_streaming: bool,
}

impl std::fmt::Debug for PendingDirectResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingDirectResponse")
            .field("required_bytes", &self.required_bytes())
            .field("is_exact", &self.is_exact())
            .field("prefers_streaming", &self.prefers_streaming)
            .finish_non_exhaustive()
    }
}

impl PendingDirectResponse {
    /// An already fully materialised wire response (no body left to read).
    fn materialized(wire: Vec<u8>) -> Self {
        Self {
            header: wire,
            prefix: Vec::new(),
            body: None,
            exact_remaining: Some(0),
            prefers_streaming: false,
        }
    }

    fn with_body(header: Vec<u8>, prefix: Vec<u8>, body: Body, prefers_streaming: bool) -> Self {
        let exact_remaining = body.size_hint().exact();
        Self {
            header,
            prefix,
            body: Some(body),
            exact_remaining,
            prefers_streaming,
        }
    }

    /// Size of the complete wire response in bytes: exact when
    /// [`Self::is_exact`], otherwise a strict lower bound (the bytes known so
    /// far plus one).  Saturates at `usize::MAX`.
    #[must_use]
    pub fn required_bytes(&self) -> usize {
        let known = self.header.len().saturating_add(self.prefix.len());
        let rest = match (&self.body, self.exact_remaining) {
            (None, _) => 0,
            (Some(_), Some(exact)) => usize::try_from(exact).unwrap_or(usize::MAX),
            (Some(_), None) => 1,
        };
        known.saturating_add(rest)
    }

    /// Whether [`Self::required_bytes`] is the exact final size.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        self.body.is_none() || self.exact_remaining.is_some()
    }

    /// Whether the response is an incremental stream (`text/event-stream`)
    /// that should be delivered through [`Self::stream`] rather than
    /// buffered — it may never end.
    #[must_use]
    pub const fn prefers_streaming(&self) -> bool {
        self.prefers_streaming
    }

    /// Continue writing the complete wire response into `out`.
    ///
    /// Returns [`ResumableDirectWrite::Complete`] once the whole response
    /// fits, or [`ResumableDirectWrite::Overflow`] with the (further read)
    /// response when `out` is still too small.  At most one body frame
    /// beyond `out` is read per call.  A configured
    /// [`crate::dispatch_timeout`] bounds the call (expiry → `504` wire), a
    /// body error yields a `500` wire response.
    pub async fn write_into(self, out: &mut [u8]) -> ResumableDirectWrite {
        let done = Deadline::start()
            .run(self.write_into_inner(&mut *out))
            .await;
        done.unwrap_or_else(|| Self::materialized(timeout_wire()).copy_into(out))
    }

    /// Copy the known bytes (header + prefix) into `out`, or report overflow.
    fn copy_known(&self, out: &mut [u8]) -> Option<usize> {
        let header_len = self.header.len();
        let known = header_len.checked_add(self.prefix.len())?;
        let dst = out.get_mut(..known)?;
        dst[..header_len].copy_from_slice(&self.header);
        dst[header_len..].copy_from_slice(&self.prefix);
        Some(known)
    }

    /// Write a body-less (materialised) response.
    fn copy_into(self, out: &mut [u8]) -> ResumableDirectWrite {
        self.copy_known(out).map_or_else(
            || ResumableDirectWrite::Overflow(self),
            ResumableDirectWrite::Complete,
        )
    }

    async fn write_into_inner(mut self, out: &mut [u8]) -> ResumableDirectWrite {
        // `required_bytes` is at least the known bytes, so a fitting
        // requirement always copies.
        let copied = (self.required_bytes() <= out.len())
            .then(|| self.copy_known(out))
            .flatten();
        let Some(mut written) = copied else {
            return ResumableDirectWrite::Overflow(self);
        };
        let Some(mut body) = self.body.take() else {
            return ResumableDirectWrite::Complete(written);
        };
        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    let Ok(data) = frame.into_data() else {
                        continue;
                    };
                    if let Some(dst) = written
                        .checked_add(data.len())
                        .and_then(|end| out.get_mut(written..end))
                    {
                        dst.copy_from_slice(&data);
                        written += data.len();
                    } else {
                        // Keep everything produced so far for the next call.
                        let mut prefix = out[self.header.len()..written].to_vec();
                        prefix.extend_from_slice(&data);
                        return ResumableDirectWrite::Overflow(Self::with_body(
                            self.header,
                            prefix,
                            body,
                            self.prefers_streaming,
                        ));
                    }
                }
                Some(Err(_)) => {
                    return Self::materialized(error_wire(500, BODY_STREAM_ERROR_MSG))
                        .copy_into(out);
                }
                None => return ResumableDirectWrite::Complete(written),
            }
        }
    }

    /// Deliver the response through header-first streaming callbacks — the
    /// same contract as [`crate::dispatch_streaming_with_header_async`]:
    /// `on_header` receives the wire header (`[u32 BE len | JSON]`) exactly
    /// once, then `on_chunk` the body (ready frames coalesced, an empty slice
    /// being a flush hint).  No total deadline applies once the header is
    /// delivered.
    pub async fn stream<H, F>(self, mut on_header: H, mut on_chunk: F) -> StreamOutcome
    where
        H: FnMut(&[u8]),
        F: FnMut(&[u8]) -> ControlFlow<()>,
    {
        on_header(&self.header);
        if !self.prefix.is_empty() && on_chunk(&self.prefix).is_break() {
            return StreamOutcome::SinkStopped;
        }
        match self.body {
            Some(body) => stream_body_tail(body, &mut on_chunk, true).await,
            None => StreamOutcome::Complete,
        }
    }

    /// Legacy (non-resumable) overflow: report the exact required size,
    /// draining the unread remainder only to count it — bounded by
    /// [`max_buffered_response_bytes`] (`500` once exceeded) so an unbounded
    /// body can no longer be drained forever.
    async fn into_exact_overflow(self, out: &mut [u8]) -> DirectWriteResult {
        let known = self.header.len().saturating_add(self.prefix.len());
        let Some(mut body) = self.body else {
            return DirectWriteResult::Overflow(known);
        };
        if let Some(exact) = self.exact_remaining {
            return DirectWriteResult::Overflow(
                known.saturating_add(usize::try_from(exact).unwrap_or(usize::MAX)),
            );
        }
        let max = max_buffered_response_bytes();
        let header_len = self.header.len();
        let mut body_len = self.prefix.len();
        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    let len = frame.data_ref().map_or(0, Bytes::len);
                    body_len = body_len.saturating_add(len);
                    if max != 0 && body_len > max {
                        return write_wire_into(out, &response_too_large_wire(max));
                    }
                }
                Some(Err(_)) => {
                    return write_wire_into(out, &error_wire(500, BODY_STREAM_ERROR_MSG));
                }
                None => return DirectWriteResult::Overflow(header_len.saturating_add(body_len)),
            }
        }
    }
}

/// Whether the response is an incremental `text/event-stream`.
fn is_event_stream(headers: &http::HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.as_bytes().get(..17))
        .is_some_and(|v| v.eq_ignore_ascii_case(b"text/event-stream"))
}

/// Legacy direct-write tail ([`crate::dispatch_into_async`] and the borrowed
/// sibling): exact-size overflow reporting, the response is discarded.
pub async fn finish_direct_write(
    out: &mut [u8],
    status: u16,
    headers: http::HeaderMap,
    metadata: ResponseMetadata,
    body: Body,
) -> DirectWriteResult {
    match finish_resumable(&mut *out, status, headers, metadata, body, false).await {
        ResumableDirectWrite::Complete(n) => DirectWriteResult::Complete(n),
        ResumableDirectWrite::Overflow(pending) => pending.into_exact_overflow(out).await,
    }
}

/// Resumable direct-write tail: `422` responses are materialised so
/// `validation_errors` hoisting is preserved byte-for-byte; every other
/// status streams status + headers + body frames straight into `out` and,
/// the moment a frame does not fit (or up front, when the body's exact size
/// hint already exceeds `out`), stops and returns the response as a
/// [`PendingDirectResponse`] — nothing beyond `out` is drained.  With
/// `stream_event_streams`, a `text/event-stream` response is returned as
/// pending immediately (it may never end, so it must not be buffered).
pub async fn finish_resumable(
    out: &mut [u8],
    status: u16,
    headers: http::HeaderMap,
    metadata: ResponseMetadata,
    mut body: Body,
    stream_event_streams: bool,
) -> ResumableDirectWrite {
    if status == 422 {
        // Bounded prefetch: a small 422 is materialised to preserve the
        // `validation_errors` hoisting byte-for-byte; an oversized one falls
        // through to the ordinary direct write below, unhoisted.
        match prefetch_422_body(body).await {
            Prefetched422::Small(bytes) => {
                // Same cap check as the buffered `finish_buffered_wire` 422
                // branch, so the two modes agree when the cap is below the
                // hoist prefetch bound (64 KiB).
                let max = max_buffered_response_bytes();
                let wire = if max != 0 && bytes.len() > max {
                    response_too_large_wire(max)
                } else {
                    to_wire_bytes((status, headers, bytes, metadata))
                };
                return PendingDirectResponse::materialized(wire).copy_into(out);
            }
            Prefetched422::Large(rest) => body = rest,
            Prefetched422::Failed => {
                return PendingDirectResponse::materialized(error_wire(500, BODY_STREAM_ERROR_MSG))
                    .copy_into(out);
            }
        }
    }

    let prefers_streaming = stream_event_streams && is_event_stream(&headers);
    // Write the wire header straight into `out` — no intermediate Vec
    // and no second copy.  `header_total` is the exact header byte count
    // whether or not it fit.
    let header_total = write_wire_header_into_slice(out, status, &headers, &metadata);
    let overflow_now = header_total > out.len()
        || prefers_streaming
        || body.size_hint().exact().is_some_and(|exact| {
            // Fast overflow: an exact-length body (a `Full` body / explicit
            // `Content-Length`) that cannot fit is reported before a single
            // frame is drained.
            u64::try_from(header_total)
                .unwrap_or(u64::MAX)
                .saturating_add(exact)
                > u64::try_from(out.len()).unwrap_or(u64::MAX)
        });
    if overflow_now {
        return ResumableDirectWrite::Overflow(PendingDirectResponse::with_body(
            build_wire_header_bytes(status, &headers, &metadata),
            Vec::new(),
            body,
            prefers_streaming,
        ));
    }

    let mut written = header_total;
    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                // `checked_add` guards the bounds test against a
                // pathological frame length wrapping `usize`.
                if let Some(dst) = written
                    .checked_add(data.len())
                    .and_then(|end| out.get_mut(written..end))
                {
                    dst.copy_from_slice(&data);
                    written += data.len();
                } else {
                    // Stop draining: keep what was produced and the unread
                    // rest of the body for the caller.
                    let mut prefix = out[header_total..written].to_vec();
                    prefix.extend_from_slice(&data);
                    return ResumableDirectWrite::Overflow(PendingDirectResponse::with_body(
                        out[..header_total].to_vec(),
                        prefix,
                        body,
                        false,
                    ));
                }
            }
            // Response body aborted mid-stream. Nothing has been committed to
            // the caller yet (we write into `out` and only return at the end),
            // so discard the partial write and emit a 500 error wire instead
            // of reporting truncated bytes as a successful response.
            Some(Err(_)) => {
                return PendingDirectResponse::materialized(error_wire(500, BODY_STREAM_ERROR_MSG))
                    .copy_into(out);
            }
            None => return ResumableDirectWrite::Complete(written),
        }
    }
}

/// [`ResumableDirectWrite`] for a fully-assembled wire response (an error
/// response, a timeout): copied into `out` when it fits, else kept pending.
pub fn resumable_from_wire(out: &mut [u8], wire: Vec<u8>) -> ResumableDirectWrite {
    PendingDirectResponse::materialized(wire).copy_into(out)
}

/// Copy a fully-assembled wire response into `out`, or report the
/// exact required size.
pub fn write_wire_into(out: &mut [u8], wire: &[u8]) -> DirectWriteResult {
    if wire.len() <= out.len() {
        out[..wire.len()].copy_from_slice(wire);
        DirectWriteResult::Complete(wire.len())
    } else {
        DirectWriteResult::Overflow(wire.len())
    }
}

#[cfg(test)]
#[path = "direct/tests.rs"]
mod tests;
