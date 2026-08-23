use std::convert::Infallible;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Once;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use axum::routing::get;
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use tokio::runtime::Builder;
use vespera_inprocess::{
    DirectWriteResult, Router, dispatch_from_bytes, dispatch_into, dispatch_streaming_async,
    register_app,
};

struct SparseFrameBody {
    index: u8,
}

impl HttpBody for SparseFrameBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let frame = match self.index {
            0 => Some(Frame::trailers(http::HeaderMap::new())),
            1 => Some(Frame::data(Bytes::new())),
            2 => Some(Frame::data(Bytes::from_static(b"payload"))),
            _ => None,
        };
        self.index += 1;
        Poll::Ready(frame.map(Ok))
    }
}

async fn sparse_response() -> Response {
    Response::new(Body::new(SparseFrameBody { index: 0 }))
}

fn install() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        register_app(|| Router::new().route("/sparse", get(sparse_response)));
    });
}

fn request_wire() -> Vec<u8> {
    let header = br#"{"v":1,"method":"GET","path":"/sparse"}"#;
    let mut wire = Vec::with_capacity(4 + header.len());
    wire.extend_from_slice(&u32::try_from(header.len()).unwrap().to_be_bytes());
    wire.extend_from_slice(header);
    wire
}

fn decode(wire: &[u8]) -> (serde_json::Value, &[u8]) {
    let header_len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header = serde_json::from_slice(&wire[4..4 + header_len]).unwrap();
    (header, &wire[4 + header_len..])
}

#[test]
fn sparse_frames_are_ignored_consistently_across_dispatch_modes() {
    install();
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let request = request_wire();

    let buffered = dispatch_from_bytes(request.clone(), &runtime);
    let (buffered_header, buffered_body) = decode(&buffered);
    assert_eq!(buffered_header["status"], 200);
    assert_eq!(buffered_body, b"payload");

    let mut direct = vec![0; buffered.len()];
    assert_eq!(
        dispatch_into(request.clone(), &mut direct, &runtime),
        DirectWriteResult::Complete(buffered.len())
    );
    assert_eq!(direct, buffered);

    let mut streamed_body = Vec::new();
    let streamed_header = runtime.block_on(dispatch_streaming_async(request, |chunk| {
        streamed_body.extend_from_slice(chunk);
        ControlFlow::Continue(())
    }));
    let (streamed_header, inline_body) = decode(&streamed_header);
    assert_eq!(streamed_header["status"], 200);
    assert_eq!(inline_body, b"");
    assert_eq!(streamed_body, b"payload");
}
