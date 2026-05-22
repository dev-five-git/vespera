//! Integration test for the `register_app` first-wins semantics:
//! a second (or later) `register_app` call must be a no-op that
//! preserves the originally registered router, without invoking the
//! supplied factory closure a second time.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::routing::get;
use vespera_inprocess::{dispatch_from_json, register_app};

#[test]
fn second_register_is_noop_first_wins() {
    let invocations = Arc::new(AtomicUsize::new(0));

    let inv = Arc::clone(&invocations);
    register_app(move || {
        inv.fetch_add(1, Ordering::SeqCst);
        Router::new().route("/from-first", get(|| async { "first" }))
    });

    let inv = Arc::clone(&invocations);
    register_app(move || {
        inv.fetch_add(100, Ordering::SeqCst);
        Router::new().route("/from-second", get(|| async { "second" }))
    });

    register_app(|| {
        unreachable!(
            "third register_app call must be a no-op without invoking the factory"
        );
    });

    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "only the first register_app should have invoked its factory; \
         later calls must short-circuit before running the closure"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    // First registration's route must be reachable.
    let response =
        dispatch_from_json(r#"{"method":"GET","path":"/from-first"}"#, &runtime);
    let v: serde_json::Value =
        serde_json::from_str(&response).expect("response is JSON");
    assert_eq!(
        v["status"].as_u64().expect("status is integer"),
        200,
        "first registration's route must still be reachable after the no-op second register_app"
    );

    // Second registration's route must NOT be reachable — the second
    // factory was never invoked so the router was never built, much less
    // installed.
    let response =
        dispatch_from_json(r#"{"method":"GET","path":"/from-second"}"#, &runtime);
    let v: serde_json::Value =
        serde_json::from_str(&response).expect("response is JSON");
    assert_eq!(
        v["status"].as_u64().expect("status is integer"),
        404,
        "second registration was a no-op — its route must not exist on the registered router"
    );
}
