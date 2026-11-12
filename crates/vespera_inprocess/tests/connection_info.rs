//! The optional `remote_addr` / `scheme` request wire-header fields reach
//! axum handlers as `ConnectInfo<SocketAddr>` / `Extension<Scheme>` on every
//! dispatch entry point (buffered, direct-write, response streaming and
//! bidirectional streaming); without them a `ConnectInfo` handler rejects
//! with `500` exactly as before.
//!
//! `ConnectInfo` insertion is compiled only with the `connect-info` feature
//! (`vespera` turns it on), so a bare `cargo test -p vespera_inprocess`
//! skips this file instead of failing it.
#![cfg(feature = "connect-info")]

use std::net::SocketAddr;
use std::ops::ControlFlow;

use axum::extract::{ConnectInfo, Extension};
use axum::http::uri::Scheme;
use axum::routing::get;
use rstest::rstest;
use vespera_inprocess::{
    DirectWriteResult, RequestChunk, Router, dispatch_bidirectional_streaming_with_header,
    dispatch_from_bytes_async, dispatch_into_async, dispatch_into_async_borrowed,
    dispatch_streaming_with_header_async, register_app_named,
};

const APP: &str = "connection_info_test";

async fn whoami(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    scheme: Option<Extension<Scheme>>,
) -> String {
    let scheme = scheme.map_or_else(|| "-".to_owned(), |Extension(s)| s.to_string());
    format!("{addr}|{scheme}")
}

fn register() {
    register_app_named(APP, || Router::new().route("/whoami", get(whoami)));
}

fn wire(extra: &str) -> Vec<u8> {
    let header = format!(r#"{{"v":1,"method":"GET","path":"/whoami","app":"{APP}"{extra}}}"#);
    let mut out = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out
}

/// `(status, body)` of a complete wire response.
fn status_and_body(wire: &[u8]) -> (u16, String) {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    let status = u16::try_from(header["status"].as_u64().unwrap()).unwrap();
    (
        status,
        String::from_utf8_lossy(&wire[4 + len..]).into_owned(),
    )
}

const WITH_CONN: &str = r#","remote_addr":"[2001:db8::1]:51000","scheme":"HTTPS""#;
const EXPECTED: &str = "[2001:db8::1]:51000|https";

#[rstest]
#[case::with_fields(WITH_CONN, 200, Some(EXPECTED))]
#[case::address_only(r#","remote_addr":"203.0.113.9:80""#, 200, Some("203.0.113.9:80|-"))]
#[case::unparseable_address_is_ignored(
    r#","remote_addr":"not-an-addr","scheme":"gopher""#,
    500,
    None
)]
#[case::absent(r"", 500, None)]
#[tokio::test]
async fn buffered_dispatch_inserts_connect_info(
    #[case] extra: &str,
    #[case] status: u16,
    #[case] body: Option<&str>,
) {
    register();
    let (got_status, got_body) = status_and_body(&dispatch_from_bytes_async(wire(extra)).await);
    assert_eq!(got_status, status, "{got_body}");
    if let Some(body) = body {
        assert_eq!(got_body, body);
    }
}

#[tokio::test]
async fn direct_write_dispatch_inserts_connect_info() {
    register();
    let mut out = vec![0u8; 4096];
    let DirectWriteResult::Complete(n) = dispatch_into_async(wire(WITH_CONN), &mut out).await
    else {
        panic!("response fits");
    };
    assert_eq!(status_and_body(&out[..n]), (200, EXPECTED.to_owned()));

    let request = wire(WITH_CONN);
    let DirectWriteResult::Complete(n) = dispatch_into_async_borrowed(&request, &mut out).await
    else {
        panic!("response fits");
    };
    assert_eq!(status_and_body(&out[..n]), (200, EXPECTED.to_owned()));
}

#[tokio::test]
async fn streaming_dispatch_inserts_connect_info() {
    register();
    let mut header = Vec::new();
    let mut body = Vec::new();
    let _outcome = dispatch_streaming_with_header_async(
        wire(WITH_CONN),
        |h: &[u8]| header.extend_from_slice(h),
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    )
    .await;
    header.extend_from_slice(&body);
    assert_eq!(status_and_body(&header), (200, EXPECTED.to_owned()));

    let mut header = Vec::new();
    let mut body = Vec::new();
    let _outcome = dispatch_bidirectional_streaming_with_header(
        wire(WITH_CONN),
        || RequestChunk::End,
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        |h: &[u8]| header.extend_from_slice(h),
    )
    .await;
    header.extend_from_slice(&body);
    assert_eq!(status_and_body(&header), (200, EXPECTED.to_owned()));
}
