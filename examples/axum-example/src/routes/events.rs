//! Server-sent events (`axum::response::Sse`).
//!
//! An `Sse<S>` return type is documented as a `text/event-stream` string
//! response. The stream here is finite so tests can read it to the end; a real
//! feed would typically be unbounded with `Sse::keep_alive`.

use std::convert::Infallible;

use futures_util::stream::{self, Stream};
use vespera::axum::response::sse::{Event, Sse};

/// Emit three `tick` events and close the stream.
#[vespera::route(get, tags = ["events"])]
pub async fn ticks() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(stream::iter((0..3).map(|n| {
        Ok(Event::default().event("tick").data(format!("tick-{n}")))
    })))
}
