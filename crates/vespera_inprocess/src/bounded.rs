//! Resource bounds shared by every dispatch path: the per-dispatch
//! [`Deadline`], the capped buffered-response sink, and the bounded `422`
//! body prefetch used for `validation_errors` hoisting.
//!
//! Kept separate from `dispatch.rs` / `streaming.rs` so the limit policy
//! (and its error messages) lives in exactly one place for the buffered,
//! direct-write and streaming finishers.

use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::{Context, Poll};

use axum::body::Body;
use bytes::{Bytes, BytesMut};
use http_body::{Body as HttpBody, Frame, SizeHint};
use http_body_util::BodyExt;

use crate::wire::error_wire;
use crate::wire::hoist::MAX_HOIST_BODY_BYTES;

/// Body of the `504` wire response produced when a dispatch exceeds
/// [`crate::dispatch_timeout`].
pub const DISPATCH_TIMEOUT_MSG: &str =
    "dispatch exceeded the configured timeout (VESPERA_DISPATCH_TIMEOUT_MS)";

/// `504 Gateway Timeout` wire response for a dispatch that exceeded its
/// deadline before anything was committed to the caller.
#[must_use]
pub fn timeout_wire() -> Vec<u8> {
    error_wire(504, DISPATCH_TIMEOUT_MSG)
}

/// `500` wire response for a buffered response body larger than
/// [`crate::max_buffered_response_bytes`].
#[must_use]
pub fn response_too_large_wire(max: usize) -> Vec<u8> {
    error_wire(500, &response_too_large_msg(max))
}

/// Message of the `500` produced for a buffered response body larger than
/// [`crate::max_buffered_response_bytes`].
#[must_use]
pub fn response_too_large_msg(max: usize) -> String {
    format!(
        "response body exceeds the configured maximum buffered size of {max} bytes \
         (VESPERA_MAX_BUFFERED_RESPONSE_BYTES); use a streaming dispatch mode for \
         large responses"
    )
}

/// Collect a response body into one [`Bytes`] for the text-envelope path,
/// capped at `max` bytes (`0` = unlimited) and grown fallibly.  The common
/// single-frame body is returned zero-copy.  `Err` carries the
/// `(status, message)` pair the envelope path reports.
pub async fn collect_capped(mut body: Body, max: usize) -> Result<Bytes, (u16, String)> {
    let mut first: Option<Bytes> = None;
    let mut joined: Vec<u8> = Vec::new();
    let mut total = 0usize;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return Err((500, crate::internal::BODY_STREAM_ERROR_MSG.to_owned()));
        };
        let Ok(data) = frame.into_data() else {
            continue; // trailers are not carried
        };
        if data.is_empty() {
            continue;
        }
        total = total.saturating_add(data.len());
        if max != 0 && total > max {
            return Err((500, response_too_large_msg(max)));
        }
        if first.is_none() && joined.is_empty() {
            first = Some(data);
            continue;
        }
        let pending = first.take();
        let extra = pending.as_ref().map_or(0, Bytes::len) + data.len();
        if joined.try_reserve(extra).is_err() {
            return Err((
                500,
                format!("response buffer of {total} bytes could not be allocated"),
            ));
        }
        if let Some(prev) = pending {
            joined.extend_from_slice(&prev);
        }
        joined.extend_from_slice(&data);
    }
    Ok(first.unwrap_or_else(|| Bytes::from(joined)))
}

/// `500` wire response for a buffered response whose buffer could not be
/// allocated (fallible `try_reserve` failed instead of aborting the host).
#[must_use]
pub fn response_alloc_failed_wire(len: usize) -> Vec<u8> {
    error_wire(
        500,
        &format!("response buffer of {len} bytes could not be allocated"),
    )
}

/// Per-dispatch deadline driven by a single Tokio [`tokio::time::Sleep`].
///
/// [`Deadline::start`] reads [`crate::dispatch_timeout`] once; when the
/// timeout is disabled (the default) no timer is created and
/// [`Deadline::run`] is a plain `.await` — the hot path pays one
/// `OnceLock` load.  When enabled, ONE timer is registered per dispatch and
/// reused across every awaited phase (pre-commit dispatch, `422` prefetch,
/// each post-commit body frame), so per-chunk streaming adds no timer
/// registrations.
pub struct Deadline {
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Deadline {
    /// Start the configured deadline (or a disabled one).
    ///
    /// Must be called from within a Tokio runtime with the time driver
    /// enabled **when a timeout is configured**.
    #[must_use]
    pub fn start() -> Self {
        let sleep = crate::config::dispatch_timeout()
            .and_then(|timeout| tokio::time::Instant::now().checked_add(timeout))
            .map(|at| Box::pin(tokio::time::sleep_until(at)));
        Self { sleep }
    }

    /// A deadline that never expires (unit tests of the streaming tails).
    #[cfg(test)]
    pub const fn disabled() -> Self {
        Self { sleep: None }
    }

    /// Drive `fut` until it completes (`Some`) or the deadline elapses
    /// (`None`, with `fut` dropped — Tokio cancellation).  Once elapsed,
    /// every later call returns `None` unless `fut` is immediately ready.
    pub async fn run<F: Future>(&mut self, fut: F) -> Option<F::Output> {
        let Some(sleep) = self.sleep.as_mut() else {
            return Some(fut.await);
        };
        let mut fut = pin!(fut);
        poll_fn(|cx| {
            if let Poll::Ready(out) = fut.as_mut().poll(cx) {
                return Poll::Ready(Some(out));
            }
            if sleep.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    }
}

/// Append `data` to the buffered response `out`, enforcing the body cap
/// (`max == 0` → unlimited) and growing the buffer **fallibly**.
///
/// `body_len` is the running body byte count (header bytes excluded).  On
/// failure the ready-to-return `500` wire response is in `Err`.
pub fn append_capped(
    out: &mut Vec<u8>,
    data: &[u8],
    body_len: &mut usize,
    max: usize,
) -> Result<(), Vec<u8>> {
    let next = body_len.saturating_add(data.len());
    if max != 0 && next > max {
        return Err(response_too_large_wire(max));
    }
    if out.try_reserve(data.len()).is_err() {
        return Err(response_alloc_failed_wire(
            out.len().saturating_add(data.len()),
        ));
    }
    out.extend_from_slice(data);
    *body_len = next;
    Ok(())
}

/// Outcome of [`prefetch_422_body`].
pub enum Prefetched422 {
    /// The whole body fit within the hoist bound — hoist from it.
    Small(Bytes),
    /// The body exceeded the hoist bound: hoisting is skipped and the body
    /// (the already-read prefix chained with the unread rest) is forwarded
    /// verbatim through the normal non-hoisting path.
    Large(Body),
    /// The body errored before the bound was reached.
    Failed,
}

/// Read at most `MAX_HOIST_BODY_BYTES + 1` bytes of a `422` body so its
/// `validation_errors` can be hoisted, **without** materialising an
/// arbitrarily large 422 body (the hoist would skip such a body anyway).
///
/// The common single-frame body (axum `Json` / `String`) is returned
/// zero-copy; only a multi-frame small body is concatenated.
pub async fn prefetch_422_body(mut body: Body) -> Prefetched422 {
    let mut frames: VecDeque<Bytes> = VecDeque::new();
    let mut first: Option<Bytes> = None;
    let mut total = 0usize;
    loop {
        match body.frame().await {
            Some(Ok(frame)) => {
                let Ok(data) = frame.into_data() else {
                    continue; // trailers: not carried by the wire format
                };
                if data.is_empty() {
                    continue;
                }
                total = total.saturating_add(data.len());
                if first.is_none() && frames.is_empty() {
                    first = Some(data);
                } else {
                    if let Some(prev) = first.take() {
                        frames.push_back(prev);
                    }
                    frames.push_back(data);
                }
                if total > MAX_HOIST_BODY_BYTES {
                    if let Some(only) = first.take() {
                        frames.push_back(only);
                    }
                    return Prefetched422::Large(Body::new(PrefixedBody {
                        prefix: frames,
                        rest: body,
                    }));
                }
            }
            Some(Err(_)) => return Prefetched422::Failed,
            None => break,
        }
    }
    if let Some(only) = first {
        return Prefetched422::Small(only);
    }
    let mut joined = BytesMut::with_capacity(total);
    for frame in frames {
        joined.extend_from_slice(&frame);
    }
    Prefetched422::Small(joined.freeze())
}

/// A [`Body`] that yields an already-read `prefix` before the unread
/// `rest` — lets an oversized `422` continue through the ordinary
/// streaming / buffered path after the bounded prefetch.
struct PrefixedBody {
    prefix: VecDeque<Bytes>,
    rest: Body,
}

impl HttpBody for PrefixedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if let Some(chunk) = self.prefix.pop_front() {
            return Poll::Ready(Some(Ok(Frame::data(chunk))));
        }
        Pin::new(&mut self.rest).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.prefix.is_empty() && self.rest.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        let queued: u64 = self
            .prefix
            .iter()
            .map(|b| u64::try_from(b.len()).unwrap_or(u64::MAX))
            .fold(0, u64::saturating_add);
        let rest = self.rest.size_hint();
        let mut hint = SizeHint::new();
        hint.set_lower(rest.lower().saturating_add(queued));
        if let Some(upper) = rest.upper() {
            hint.set_upper(upper.saturating_add(queued));
        }
        hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(fut)
    }

    fn status_of(wire: &[u8]) -> u64 {
        let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
        let header: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
        header["status"].as_u64().unwrap()
    }

    #[rstest]
    #[case::unlimited(0, &[b"abc".as_slice(), b"defg"], Ok(7))]
    #[case::exactly_at_cap(7, &[b"abc".as_slice(), b"defg"], Ok(7))]
    #[case::one_over_cap(6, &[b"abc".as_slice(), b"defg"], Err(500))]
    fn append_capped_enforces_body_cap(
        #[case] max: usize,
        #[case] chunks: &[&[u8]],
        #[case] expected: Result<usize, u64>,
    ) {
        let mut out = b"HDR".to_vec();
        let mut body_len = 0;
        let mut result = Ok(());
        for chunk in chunks {
            result = append_capped(&mut out, chunk, &mut body_len, max);
            if result.is_err() {
                break;
            }
        }
        match (result, expected) {
            (Ok(()), Ok(len)) => {
                assert_eq!(body_len, len);
                assert_eq!(out.len(), 3 + len, "header bytes are not counted");
            }
            (Err(wire), Err(status)) => {
                assert_eq!(status_of(&wire), status);
                let text = String::from_utf8_lossy(&wire);
                assert!(
                    text.contains("VESPERA_MAX_BUFFERED_RESPONSE_BYTES"),
                    "{text}"
                );
            }
            (got, want) => panic!("got {got:?}, want {want:?}"),
        }
    }

    fn multi_frame_body(frames: Vec<Vec<u8>>) -> Body {
        let stream = futures_util::stream::iter(
            frames
                .into_iter()
                .map(|f| Ok::<_, std::convert::Infallible>(Bytes::from(f))),
        );
        Body::from_stream(stream)
    }

    #[test]
    fn prefetch_small_single_frame_is_returned_whole() {
        let Prefetched422::Small(bytes) =
            block_on(prefetch_422_body(Body::from(r#"{"errors":[]}"#)))
        else {
            panic!("small body must prefetch whole");
        };
        assert_eq!(&bytes[..], br#"{"errors":[]}"#);
    }

    #[test]
    fn prefetch_small_multi_frame_is_concatenated() {
        let body = multi_frame_body(vec![b"ab".to_vec(), Vec::new(), b"cd".to_vec()]);
        let Prefetched422::Small(bytes) = block_on(prefetch_422_body(body)) else {
            panic!("small body must prefetch whole");
        };
        assert_eq!(&bytes[..], b"abcd");
    }

    #[test]
    fn prefetch_large_body_forwards_every_byte_unhoisted() {
        let half = MAX_HOIST_BODY_BYTES / 2 + 1;
        let frames = vec![vec![b'a'; half], vec![b'b'; half], vec![b'c'; 10]];
        let expected: Vec<u8> = frames.concat();
        let body = multi_frame_body(frames);
        let Prefetched422::Large(body) = block_on(prefetch_422_body(body)) else {
            panic!("oversized 422 must not be materialised for hoisting");
        };
        let collected = block_on(body.collect()).expect("forwarded body").to_bytes();
        assert_eq!(collected.len(), expected.len());
        assert_eq!(&collected[..], &expected[..]);
    }

    #[test]
    fn deadline_disabled_by_default_runs_to_completion() {
        let mut deadline = Deadline::disabled();
        assert_eq!(block_on(deadline.run(async { 7 })), Some(7));
    }

    #[test]
    fn deadline_elapses_and_stays_elapsed() {
        block_on(async {
            let mut deadline = Deadline {
                sleep: Some(Box::pin(tokio::time::sleep(
                    std::time::Duration::from_millis(5),
                ))),
            };
            assert_eq!(deadline.run(std::future::pending::<()>()).await, None);
            assert_eq!(
                deadline.run(std::future::pending::<()>()).await,
                None,
                "an elapsed deadline stays elapsed"
            );
            assert_eq!(
                deadline.run(async { 1 }).await,
                Some(1),
                "an immediately-ready future still wins"
            );
        });
    }

    #[test]
    fn timeout_wire_is_504() {
        assert_eq!(status_of(&timeout_wire()), 504);
    }
}
