//! Request-header count cap: `http::HeaderMap` panics past 32 768 entries,
//! so every entry point must reject an oversized header set with a `431`
//! wire response instead of panicking (wire parser cap +
//! `try_reserve`/`try_append` in the request builder).

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::routing::get;
use rstest::rstest;
use vespera_inprocess::{
    DirectWriteResult, MAX_REQUEST_HEADERS, RequestEnvelope, Router, dispatch_from_bytes_async,
    dispatch_into_async_borrowed, dispatch_owned, register_app_named,
};

const APP: &str = "header_count_limit";

fn ensure_app() {
    register_app_named(APP, || Router::new().route("/n", get(|| async { "ok" })));
}

fn wire_with_headers(count: usize) -> Vec<u8> {
    let mut json = format!(r#"{{"v":1,"method":"GET","path":"/n","app":"{APP}","headers":{{"#);
    for i in 0..count {
        if i > 0 {
            json.push(',');
        }
        let _ = write!(json, r#""x-h{i}":"v""#);
    }
    json.push_str("}}");
    let mut wire = u32::try_from(json.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(json.as_bytes());
    wire
}

fn status(wire: &[u8]) -> u64 {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    header["status"].as_u64().unwrap()
}

#[rstest]
#[case::at_the_cap(MAX_REQUEST_HEADERS, 200)]
#[case::one_over(MAX_REQUEST_HEADERS + 1, 431)]
#[case::past_header_map_max_size(40_000, 431)]
#[tokio::test]
async fn buffered_and_direct_paths_answer_431(#[case] count: usize, #[case] expected: u64) {
    ensure_app();
    let wire = wire_with_headers(count);
    assert_eq!(
        status(&dispatch_from_bytes_async(wire.clone()).await),
        expected
    );

    let mut out = vec![0u8; 4096];
    let DirectWriteResult::Complete(n) = dispatch_into_async_borrowed(&wire, &mut out).await else {
        panic!("small response must fit");
    };
    assert_eq!(status(&out[..n]), expected);
}

#[tokio::test]
async fn envelope_api_answers_431_instead_of_panicking() {
    let headers: HashMap<String, String> = (0..40_000)
        .map(|i| (format!("x-h{i}"), "v".to_owned()))
        .collect();
    let envelope = RequestEnvelope {
        principal: None,
        method: "GET".to_owned(),
        path: "/n".to_owned(),
        query: String::new(),
        headers,
        body: String::new(),
    };
    let router = Router::new().route("/n", get(|| async { "ok" }));
    let response = dispatch_owned(router, envelope).await;
    assert_eq!(response.status, 431);
}
