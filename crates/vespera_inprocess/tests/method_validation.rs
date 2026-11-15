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
        principal: None,
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
    let response =
        dispatch_typed(router_with_get_test(), &envelope_with_method("BAD METHOD")).await;
    assert_eq!(response.status, 405);
    assert!(
        response.body.contains("BAD METHOD"),
        "405 body should mention the offending method, got: {body}",
        body = response.body,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn empty_method_returns_405() {
    let response = dispatch_typed(router_with_get_test(), &envelope_with_method("")).await;
    assert_eq!(response.status, 405);
}

#[tokio::test(flavor = "current_thread")]
async fn method_with_control_char_returns_405() {
    let response = dispatch_typed(router_with_get_test(), &envelope_with_method("GET\n")).await;
    assert_eq!(response.status, 405);
}

#[tokio::test(flavor = "current_thread")]
async fn valid_method_dispatches_normally() {
    // Sanity check: a real GET still reaches the handler.  The 405
    // short-circuit must not regress the happy path.
    let response = dispatch_typed(router_with_get_test(), &envelope_with_method("GET")).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "would-have-been-wrong");
}

/// An invalid method is client text of up to the header cap: the `405` body
/// (and the WARN event built from it) echoes at most a 64-byte prefix, cut on
/// a char boundary and marked `...`.
#[tokio::test(flavor = "current_thread")]
async fn long_invalid_method_is_truncated_in_the_405_body() {
    let prefix = "B".repeat(60);
    // 62 ASCII bytes + a 3-byte char straddling the 64-byte cut.
    let method = format!("{prefix} x\u{20ac}tail");
    let response = dispatch_typed(router_with_get_test(), &envelope_with_method(&method)).await;
    assert_eq!(response.status, 405);
    assert_eq!(
        response.body,
        format!("Method Not Allowed: '{prefix} x...' is not a valid HTTP method")
    );
    // Exactly 64 bytes: echoed whole, no marker.
    let method = format!("{prefix} xyz");
    let response = dispatch_typed(router_with_get_test(), &envelope_with_method(&method)).await;
    assert_eq!(
        response.body,
        format!("Method Not Allowed: '{method}' is not a valid HTTP method")
    );
}
