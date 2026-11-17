//! The outcome a header-first streaming dispatch reports, and the canonical
//! error wire for a truncated response.

use crate::internal::{BODY_SINK_STOPPED_MSG, BODY_STREAM_ERROR_MSG};
use crate::wire::error_wire_untraced;

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
    ///
    /// Builds the wire without the `tracing` error event: the dispatcher
    /// already reported the truncation inside its `vespera.dispatch` span
    /// (a sink stop — the client went away — is not an engine error).
    #[must_use]
    pub fn error_wire(self) -> Vec<u8> {
        match self {
            Self::SinkStopped => error_wire_untraced(500, BODY_SINK_STOPPED_MSG),
            Self::Complete | Self::BodyError => error_wire_untraced(500, BODY_STREAM_ERROR_MSG),
        }
    }
}
