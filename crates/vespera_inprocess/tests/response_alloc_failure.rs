//! A buffered response whose body announces a pathological exact size
//! (`u64::MAX`) with the buffered-response cap disabled must degrade to a
//! `500` wire response — the up-front `try_reserve_exact` fails instead of
//! aborting the host process on allocation failure.
//!
//! Own test binary: the cap is a first-wins process-global `OnceLock`, and
//! this test needs it disabled (`0`) before the first dispatch.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use bytes::Bytes;
use http_body::{Frame, SizeHint};
use tokio::runtime::Builder;
use vespera_inprocess::{dispatch_from_bytes, register_app, set_max_buffered_response_bytes};

/// Claims an exact length of `u64::MAX` bytes but never yields a frame.
struct ImpossiblyLargeBody;

impl http_body::Body for ImpossiblyLargeBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(u64::MAX)
    }
}

#[test]
fn unallocatable_buffered_response_becomes_500() {
    assert!(
        set_max_buffered_response_bytes(0),
        "the cap must not be fixed before this test"
    );
    register_app(|| Router::new().route("/huge", get(|| async { Body::new(ImpossiblyLargeBody) })));
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");

    let header = br#"{"v":1,"method":"GET","path":"/huge"}"#;
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(header);
    let resp = dispatch_from_bytes(wire, &runtime);

    let header_len = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    let json: serde_json::Value =
        serde_json::from_slice(&resp[4..4 + header_len]).expect("response header JSON");
    assert_eq!(json["status"], 500);
    let body = String::from_utf8_lossy(&resp[4 + header_len..]);
    assert!(body.contains("could not be allocated"), "{body}");
}
