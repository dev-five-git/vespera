//! `POST /upload` maps a multipart rejection to its own status: a body over
//! axum's 2 MiB default limit is `413`, a malformed one `400`.

use rust_jni_demo::create_app;
use testkit::{MultipartForm, Part, TestServer};
use vespera::axum::{body::Bytes, http::StatusCode};

#[tokio::test]
async fn oversized_upload_answers_413() {
    let server = TestServer::new(create_app());
    let big = Part::bytes(vec![0u8; 3 * 1024 * 1024]).file_name("big.bin");
    let response = server
        .post("/upload")
        .multipart(MultipartForm::new().add_part("file", big))
        .await;
    response.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn malformed_upload_answers_400() {
    let server = TestServer::new(create_app());
    let response = server
        .post("/upload")
        .content_type("multipart/form-data; boundary=missing")
        .bytes(Bytes::from_static(b"not a multipart body"))
        .await;
    response.assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn upload_declares_both_rejection_statuses() {
    let spec = TestServer::new(create_app()).get("/openapi.json").await;
    let responses = &spec.json::<serde_json::Value>()["paths"]["/upload"]["post"]["responses"];
    assert!(responses.get("400").is_some(), "{responses}");
    assert!(responses.get("413").is_some(), "{responses}");
    assert!(responses.get("500").is_some(), "{responses}");
}
