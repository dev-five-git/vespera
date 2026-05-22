//! Integration tests for the malformed-HTTP-method correctness fix:
//! invalid method strings now short-circuit to `405 Method Not Allowed`
//! instead of being silently coerced to `GET` (which would dispatch the
//! request to the wrong handler).

use std::collections::HashMap;

use axum::Router;
use axum::routing::get;
use vespera_inprocess::{RequestEnvelope, dispatch_typed};

fn envelope_with_method(method: &str) -> RequestEnvelope {
    RequestEnvelope {
        method: method.to_owned(),
        path: "/test".to_owned(),
        query: String::new(),
        headers: HashMap::new(),
        body: String::new(),
    }
}

fn router_with_get_test() -> Router {
    Router::new().route("/test", get(|| async { "would-have-been-wrong" }))
}

#[tokio::test(flavor = "current_thread")]
async fn method_with_space_returns_405() {
    // Before the fix, "BAD METHOD" was silently coerced to GET and the
    // request hit the GET handler at /test with status 200.
    let response = dispatch_typed(
        router_with_get_test(),
        &envelope_with_method("BAD METHOD"),
    )
    .await;
    assert_eq!(response.status, 405);
    assert!(
        response.body.contains("BAD METHOD"),
        "405 body should mention the offending method, got: {body}",
        body = response.body,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn empty_method_returns_405() {
    let response =
        dispatch_typed(router_with_get_test(), &envelope_with_method("")).await;
    assert_eq!(response.status, 405);
}

#[tokio::test(flavor = "current_thread")]
async fn method_with_control_char_returns_405() {
    let response =
        dispatch_typed(router_with_get_test(), &envelope_with_method("GET\n")).await;
    assert_eq!(response.status, 405);
}

#[tokio::test(flavor = "current_thread")]
async fn valid_method_dispatches_normally() {
    // Sanity check: a real GET still reaches the handler.  The 405
    // short-circuit must not regress the happy path.
    let response =
        dispatch_typed(router_with_get_test(), &envelope_with_method("GET")).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "would-have-been-wrong");
}
