//! Runtime behaviour of the `routes/shapes.rs` handlers whose OpenAPI shapes
//! (struct path params, wildcard, query flatten/skip/repeat, opaque / HTML /
//! binary bodies, optional JSON body, explicit verbatim paths) are pinned by
//! the `openapi.json` snapshot.

use axum_example::create_app;
use serde_json::json;
use testkit::TestServer;
use vespera::axum::{body::Bytes, http::StatusCode};

#[tokio::test]
async fn struct_path_and_wildcard_segments() {
    let server = TestServer::new(create_app().await);

    server
        .get("/shapes/items/7/hello")
        .await
        .assert_text("7:hello");
    server
        .get("/shapes/files/a/b/c.txt")
        .await
        .assert_text("a/b/c.txt");
}

#[tokio::test]
async fn query_repeats_keys_skips_and_flattens() {
    let server = TestServer::new(create_app().await);

    server
        .get("/shapes/search?q=x&tags=a&tags=b&cursor=3&internal=9")
        .await
        .assert_text("x|a,b|0|3");
    server.get("/shapes/search?q=y").await.assert_text("y||0|");
}

#[tokio::test]
async fn html_bytes_redirect_and_opaque_bodies() {
    let server = TestServer::new(create_app().await);

    let html = server.get("/shapes/html").await;
    html.assert_status_ok();
    assert!(
        html.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );

    let bytes = server.get("/shapes/bytes").await;
    assert_eq!(bytes.headers()["content-type"], "application/octet-stream");
    assert_eq!(bytes.text().as_bytes(), [0, 1, 2]);

    let redirect = server.get("/shapes/redirect").await;
    redirect.assert_status(StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(redirect.headers()["location"], "/health");

    server.get("/shapes/opaque").await.assert_text("opaque");
    server.get("/shapes/raw_path").await.assert_text("raw");
    server
        .get("/shapes/raw-path")
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn binary_and_optional_json_request_bodies() {
    let server = TestServer::new(create_app().await);

    let upload = server
        .post("/shapes/upload")
        .content_type("application/octet-stream")
        .bytes(Bytes::from_static(b"\x00\x01\x02\x03"))
        .await;
    assert_eq!(upload.json::<usize>(), 4);

    server
        .post("/shapes/note")
        .json(&json!({ "text": "hi" }))
        .await
        .assert_text("hi");
    server.post("/shapes/note").await.assert_text("none");
}

#[tokio::test]
async fn telemetry_serializes_documented_wire_shapes() {
    let server = TestServer::new(create_app().await);

    let response = server
        .post("/shapes/telemetry")
        .json(&json!({ "name": "n" }))
        .await;
    assert_eq!(
        response.json::<serde_json::Value>(),
        json!({
            "payload": { "name": "n" },
            "elapsed": { "secs": 1, "nanos": 500_000_000 },
            "event": { "kind": "job_finished", "jobId": 7, "exitCode": 0 }
        })
    );

    server
        .post("/shapes/telemetry")
        .json(&json!({ "name": "n", "extra": true }))
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}
