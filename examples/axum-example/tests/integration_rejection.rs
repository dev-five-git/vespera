//! `Result<Extractor, Rejection>` handler arguments: the handler receives the
//! rejection, and OpenAPI still documents the body / query parameters.

use axum_example::create_app;
use testkit::TestServer;
use vespera::axum::http::StatusCode;

#[tokio::test]
async fn result_json_body_dispatches_and_hands_the_rejection_to_the_handler() {
    let server = TestServer::new(create_app().await);

    let ok = server
        .post("/rejection")
        .json(&serde_json::json!({ "name": "vespera" }))
        .await;
    ok.assert_status_ok();
    assert_eq!(ok.json::<serde_json::Value>()["name"], "vespera");

    let rejected = server
        .post("/rejection")
        .content_type("application/json")
        .bytes("{".into())
        .await;
    rejected.assert_status(StatusCode::BAD_REQUEST);

    // axum's own status per rejection kind: a wrong shape is 422, a missing
    // JSON content type is 415.
    let wrong_shape = server
        .post("/rejection")
        .content_type("application/json")
        .bytes(r#"{"name": 5}"#.into())
        .await;
    wrong_shape.assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    let no_json = server
        .post("/rejection")
        .content_type("text/plain")
        .bytes(r#"{"name": "x"}"#.into())
        .await;
    no_json.assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn result_query_dispatches_and_hands_the_rejection_to_the_handler() {
    let server = TestServer::new(create_app().await);

    server
        .get("/rejection?q=a&limit=3")
        .await
        .assert_text("a:3");
    let rejected = server.get("/rejection").await;
    rejected.assert_status(StatusCode::BAD_REQUEST);
    rejected.assert_text("invalid query");
}

/// A body over axum's default 2 MiB limit is `BytesRejection`, which axum maps
/// to `413`: the handler answers it with that status, so it is documented.
#[tokio::test]
async fn oversized_json_body_is_a_413_rejection() {
    let server = TestServer::new(create_app().await);
    let oversized = format!(r#"{{"name": "{}"}}"#, "x".repeat(2 * 1024 * 1024));

    let response = server
        .post("/rejection")
        .content_type("application/json")
        .bytes(oversized.into())
        .await;

    response.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn result_extractors_are_documented() {
    let server = TestServer::new(create_app().await);
    let spec: serde_json::Value = server.get("/openapi.json").await.json();

    let post = &spec["paths"]["/rejection"]["post"];
    assert_eq!(post["requestBody"]["required"], true);
    assert_eq!(
        post["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/RejectionInput"
    );

    let params: Vec<(&str, &str, bool)> = spec["paths"]["/rejection"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap(),
                p["in"].as_str().unwrap(),
                p["required"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(params, [("q", "query", true), ("limit", "query", false)]);
}

/// `POST /form/upload` answers a malformed multipart body with `400` (an
/// oversized one with `413`) instead of panicking the handler (which dropped
/// the connection with no response).
#[tokio::test]
async fn raw_multipart_upload_rejects_bad_bodies_with_their_status() {
    let server = TestServer::new(create_app().await);

    let ok = server
        .post("/form/upload")
        .multipart(testkit::MultipartForm::new().add_text("note", "hello"))
        .await;
    ok.assert_status_ok();
    assert_eq!(ok.json::<serde_json::Value>()["ticketId"], "UPLOAD-001");

    let malformed = server
        .post("/form/upload")
        .content_type("multipart/form-data; boundary=x")
        .bytes("garbage".into())
        .await;
    malformed.assert_status(StatusCode::BAD_REQUEST);

    // Over axum's default 2 MiB body limit: `413`, as `MultipartError`
    // reports it (not a blanket `400`).
    let oversized = server
        .post("/form/upload")
        .multipart(testkit::MultipartForm::new().add_text("note", "x".repeat(3 << 20)))
        .await;
    oversized.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
}

/// `POST /error/cookie-jar-logout` clears the `session` cookie with an
/// expiring `Set-Cookie`, as its summary says.
#[tokio::test]
async fn cookie_jar_logout_expires_the_session_cookie() {
    let server = TestServer::new(create_app().await);
    let response = server
        .post("/error/cookie-jar-logout")
        .add_header(vespera::axum::http::header::COOKIE, "session=abc")
        .await;
    response.assert_status_ok();
    let set_cookie = response.headers()[vespera::axum::http::header::SET_COOKIE]
        .to_str()
        .unwrap();
    assert!(set_cookie.starts_with("session=;"), "{set_cookie}");
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
    // Same path as the `Path=/` session cookie, else the browser keeps it.
    assert!(
        set_cookie.split("; ").any(|attr| attr == "Path=/"),
        "{set_cookie}"
    );
}
