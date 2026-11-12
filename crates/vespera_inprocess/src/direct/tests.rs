use std::convert::Infallible;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use bytes::Bytes;
use http_body::Frame;
use rstest::rstest;

use super::*;

fn frames(parts: &[&'static [u8]]) -> Body {
    let frames: Vec<Result<Bytes, Infallible>> =
        parts.iter().map(|p| Ok(Bytes::from_static(p))).collect();
    Body::from_stream(futures_util::stream::iter(frames))
}

/// One step of a [`Script`] body.
#[derive(Clone, Copy)]
enum S {
    Data(&'static [u8]),
    Trailers,
    Fail,
}

/// Unknown-length body replaying `steps`.
struct Script(Vec<S>);

impl HttpBody for Script {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        if self.0.is_empty() {
            return Poll::Ready(None);
        }
        Poll::Ready(Some(match self.0.remove(0) {
            S::Data(d) => Ok(Frame::data(Bytes::from_static(d))),
            S::Trailers => Ok(Frame::trailers(http::HeaderMap::new())),
            S::Fail => Err(std::io::Error::other("boom")),
        }))
    }
}

fn script(steps: &[S]) -> Body {
    Body::new(Script(steps.to_vec()))
}

fn header_len() -> usize {
    build_wire_header_bytes(200, &http::HeaderMap::new(), &ResponseMetadata::current()).len()
}

fn status(wire: &[u8]) -> u64 {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let json: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    json["status"].as_u64().unwrap()
}

async fn resumable(out: &mut [u8], headers: http::HeaderMap, body: Body) -> ResumableDirectWrite {
    finish_resumable(out, 200, headers, ResponseMetadata::current(), body, true).await
}

async fn legacy(out: &mut [u8], body: Body) -> DirectWriteResult {
    finish_direct_write(
        out,
        200,
        http::HeaderMap::new(),
        ResponseMetadata::current(),
        body,
    )
    .await
}

/// Reference bytes: the buffered wire for the same response.
fn expected_wire(body: &[u8]) -> Vec<u8> {
    to_wire_bytes((
        200,
        http::HeaderMap::new(),
        Bytes::copy_from_slice(body),
        ResponseMetadata::current(),
    ))
}

#[tokio::test]
async fn unknown_length_overflow_keeps_the_response_and_resumes() {
    let mut out = vec![0u8; header_len() + 3];
    let ResumableDirectWrite::Overflow(pending) = resumable(
        &mut out,
        http::HeaderMap::new(),
        frames(&[b"ab", b"cd", b"ef"]),
    )
    .await
    else {
        panic!("6 body bytes do not fit 3");
    };
    assert!(!pending.is_exact());
    assert!(!pending.prefers_streaming());
    assert_eq!(pending.required_bytes(), header_len() + 4 + 1);
    assert!(format!("{pending:?}").contains("PendingDirectResponse"));

    // Still too small: one more frame is read, nothing is lost.
    let mut small = vec![0u8; header_len() + 5];
    let ResumableDirectWrite::Overflow(pending) = pending.write_into(&mut small).await else {
        panic!("still too small");
    };
    assert_eq!(pending.required_bytes(), header_len() + 6 + 1);

    // Below even the known bytes: no progress, the response is kept.
    let mut tiny = vec![0u8; 4];
    let ResumableDirectWrite::Overflow(pending) = pending.write_into(&mut tiny).await else {
        panic!("tiny");
    };

    let mut big = vec![0u8; 4096];
    let ResumableDirectWrite::Complete(n) = pending.write_into(&mut big).await else {
        panic!("fits now");
    };
    assert_eq!(&big[..n], expected_wire(b"abcdef").as_slice());
}

#[tokio::test]
async fn exact_size_overflow_is_reported_before_draining() {
    let mut out = vec![0u8; header_len()];
    let ResumableDirectWrite::Overflow(pending) =
        resumable(&mut out, http::HeaderMap::new(), Body::from("payload")).await
    else {
        panic!("exact body does not fit");
    };
    assert!(pending.is_exact());
    assert_eq!(pending.required_bytes(), header_len() + 7);
    let mut exact = vec![0u8; pending.required_bytes()];
    let ResumableDirectWrite::Complete(n) = pending.write_into(&mut exact).await else {
        panic!("exact buffer fits");
    };
    assert_eq!(&exact[..n], expected_wire(b"payload").as_slice());

    assert_eq!(
        legacy(&mut out, Body::from("payload")).await,
        DirectWriteResult::Overflow(header_len() + 7)
    );
}

#[tokio::test]
async fn header_larger_than_out_is_pending() {
    let mut out = vec![0u8; 3];
    let ResumableDirectWrite::Overflow(pending) =
        resumable(&mut out, http::HeaderMap::new(), Body::empty()).await
    else {
        panic!("header does not fit");
    };
    assert_eq!(pending.required_bytes(), header_len());
}

#[rstest]
#[case::sse("text/event-stream; charset=utf-8", true)]
#[case::upper("Text/Event-Stream", true)]
#[case::json("application/json", false)]
#[case::short("text/plain", false)]
fn event_stream_detection(#[case] content_type: &str, #[case] expected: bool) {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_str(content_type).unwrap(),
    );
    assert_eq!(is_event_stream(&headers), expected);
    assert!(!is_event_stream(&http::HeaderMap::new()));
}

#[tokio::test]
async fn event_stream_is_pending_immediately_and_streams() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("text/event-stream"),
    );
    let mut out = vec![0u8; 4096];
    let ResumableDirectWrite::Overflow(pending) =
        resumable(&mut out, headers, frames(&[b"data: 1\n\n"])).await
    else {
        panic!("event streams are never buffered");
    };
    assert!(pending.prefers_streaming());
    let mut header = Vec::new();
    let mut body = Vec::new();
    let outcome = pending
        .stream(
            |h: &[u8]| header.extend_from_slice(h),
            |c: &[u8]| {
                body.extend_from_slice(c);
                ControlFlow::Continue(())
            },
        )
        .await;
    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(status(&header), 200);
    assert_eq!(body, b"data: 1\n\n");
}

#[rstest]
#[case::delivers_prefix_and_rest(false, StreamOutcome::Complete, b"abcdef".as_slice())]
#[case::stop_on_prefix(true, StreamOutcome::SinkStopped, b"abcd".as_slice())]
#[tokio::test]
async fn pending_streams_prefix_then_rest(
    #[case] stop: bool,
    #[case] expected: StreamOutcome,
    #[case] expected_body: &[u8],
) {
    let mut out = vec![0u8; header_len() + 3];
    let ResumableDirectWrite::Overflow(pending) = resumable(
        &mut out,
        http::HeaderMap::new(),
        frames(&[b"ab", b"cd", b"ef"]),
    )
    .await
    else {
        panic!("overflow");
    };
    let mut body = Vec::new();
    let outcome = pending
        .stream(
            |_h: &[u8]| {},
            |c: &[u8]| {
                body.extend_from_slice(c);
                if stop {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        )
        .await;
    assert_eq!(outcome, expected);
    assert_eq!(body, expected_body);
}

#[tokio::test]
async fn materialized_pending_streams_and_writes_without_a_body() {
    // A small 422 that does not fit is kept materialised.
    let mut tiny = vec![0u8; 4];
    let ResumableDirectWrite::Overflow(pending) = finish_resumable(
        &mut tiny,
        422,
        http::HeaderMap::new(),
        ResponseMetadata::current(),
        Body::from(r#"{"errors":[]}"#),
        true,
    )
    .await
    else {
        panic!("does not fit");
    };
    assert!(pending.is_exact());
    let mut header = Vec::new();
    let outcome = pending
        .stream(
            |h: &[u8]| header.extend_from_slice(h),
            |_c: &[u8]| ControlFlow::Continue(()),
        )
        .await;
    assert_eq!(outcome, StreamOutcome::Complete);
    assert_eq!(status(&header), 422);

    let ResumableDirectWrite::Overflow(pending) =
        resumable_from_wire(&mut tiny, error_wire(500, "x"))
    else {
        panic!("does not fit");
    };
    let mut big = vec![0u8; 512];
    let ResumableDirectWrite::Complete(n) = pending.write_into(&mut big).await else {
        panic!("fits");
    };
    assert_eq!(status(&big[..n]), 500);
}

#[rstest]
#[case::ok(false, 200)]
#[case::body_error(true, 500)]
#[tokio::test]
async fn trailers_are_skipped_and_body_errors_become_500(
    #[case] fail: bool,
    #[case] expected: u64,
) {
    let steps: Vec<S> = if fail {
        vec![S::Trailers, S::Data(b"0123456789"), S::Trailers, S::Fail]
    } else {
        vec![S::Trailers, S::Data(b"0123456789"), S::Trailers]
    };
    // Direct path (fits).
    let mut out = vec![0u8; 4096];
    let ResumableDirectWrite::Complete(n) =
        resumable(&mut out, http::HeaderMap::new(), script(&steps)).await
    else {
        panic!("fits");
    };
    assert_eq!(status(&out[..n]), expected);

    // Resume path: the header fits, the data frame does not.
    let mut small = vec![0u8; header_len() + 1];
    let ResumableDirectWrite::Overflow(pending) =
        resumable(&mut small, http::HeaderMap::new(), script(&steps)).await
    else {
        panic!("overflow");
    };
    let ResumableDirectWrite::Complete(n) = pending.write_into(&mut out).await else {
        panic!("fits");
    };
    assert_eq!(status(&out[..n]), expected);

    // Legacy path: overflow, then the counting drain sees the rest.
    let result = legacy(&mut small, script(&steps)).await;
    if fail {
        let DirectWriteResult::Overflow(needed) = result else {
            panic!("the 500 does not fit a header+1 buffer either");
        };
        assert!(needed > small.len());
        let DirectWriteResult::Complete(n) = legacy(
            &mut out[..600],
            script(&[S::Data(&[b'x'; 1024]), S::Trailers, S::Fail]),
        )
        .await
        else {
            panic!("the 500 fits");
        };
        assert_eq!(status(&out[..n]), 500);
    } else {
        assert_eq!(result, DirectWriteResult::Overflow(header_len() + 10));
    }
}

#[tokio::test]
async fn legacy_overflow_of_a_materialised_response_reports_its_size() {
    let mut tiny = vec![0u8; 4];
    let wire = to_wire_bytes((
        422,
        http::HeaderMap::new(),
        Bytes::from_static(br#"{"errors":[]}"#),
        ResponseMetadata::current(),
    ));
    let result = finish_direct_write(
        &mut tiny,
        422,
        http::HeaderMap::new(),
        ResponseMetadata::current(),
        Body::from(r#"{"errors":[]}"#),
    )
    .await;
    assert_eq!(result, DirectWriteResult::Overflow(wire.len()));
}
