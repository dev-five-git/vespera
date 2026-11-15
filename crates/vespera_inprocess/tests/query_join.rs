//! A request whose `path` already carries a query (the Java bridge embeds it,
//! `/items?page=2`) AND a separate `query` field must join the two with `&`,
//! never with a second `?`: `/items?page=2?limit=10` is a valid URI that
//! `Query<..>` decodes as `page = "2?limit=10"` with no `limit` at all.

use std::collections::BTreeMap;
use std::sync::Once;

use axum::Router;
use axum::extract::Query;
use axum::routing::get;
use rstest::rstest;
use serde_json::{Value, json};
use tokio::runtime::Builder;
use vespera_inprocess::{RequestEnvelope, dispatch, dispatch_from_bytes, register_app_named};

const APP: &str = "query_join_app";

/// Echoes the raw query string and the decoded pairs.
async fn echo(uri: axum::http::Uri, Query(pairs): Query<BTreeMap<String, String>>) -> String {
    json!({ "raw": uri.query(), "pairs": pairs }).to_string()
}

fn install() {
    static INIT: Once = Once::new();
    INIT.call_once(|| register_app_named(APP, || Router::new().route("/items", get(echo))));
}

fn wire(path: &str, query: &str) -> Vec<u8> {
    let header = json!({ "v": 1, "method": "GET", "path": path, "query": query, "app": APP });
    let header = serde_json::to_vec(&header).unwrap();
    let mut out = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&header);
    out
}

fn body_json(resp: &[u8]) -> (u64, Value) {
    let n = u32::from_be_bytes(resp[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&resp[4..4 + n]).unwrap();
    let body: Value = serde_json::from_slice(&resp[4 + n..]).unwrap();
    (header["status"].as_u64().unwrap(), body)
}

#[rstest]
#[case::query_field_only("/items", "page=2&limit=10", "page=2&limit=10")]
#[case::path_query_only("/items?page=2&limit=10", "", "page=2&limit=10")]
#[case::both_joined_with_ampersand("/items?page=2", "limit=10", "page=2&limit=10")]
#[case::path_ends_with_question_mark("/items?", "page=2&limit=10", "page=2&limit=10")]
#[case::path_ends_with_ampersand("/items?page=2&", "limit=10", "page=2&limit=10")]
fn path_query_and_query_field_compose(#[case] path: &str, #[case] query: &str, #[case] raw: &str) {
    install();
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    let expected = json!({ "raw": raw, "pairs": { "page": "2", "limit": "10" } });

    let (status, body) = body_json(&dispatch_from_bytes(wire(path, query), &rt));
    assert_eq!(status, 200);
    assert_eq!(body, expected, "wire path");

    // The text-envelope API composes the URI through the same helper.
    let router = Router::new().route("/items", get(echo));
    let envelope = RequestEnvelope {
        method: "GET".to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        ..RequestEnvelope::default()
    };
    let response: Value = serde_json::from_str(&rt.block_on(dispatch(router, &envelope))).unwrap();
    assert_eq!(response["status"], 200);
    let body: Value = serde_json::from_str(response["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, expected, "envelope path");
}
