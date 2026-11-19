//! `TypedHeader<Authorization<Bearer>>` on `GET /typed-header`: a bearer
//! header is parsed, a missing one is rejected with `400` (round-27
//! macrorest-7 L4: the route parses the header only, it verifies nothing).

use axum_example::create_app;
use testkit::TestServer;
use vespera::axum::http::{StatusCode, header::AUTHORIZATION};

#[tokio::test]
async fn typed_header_jwt_rejects_a_request_without_authorization() {
    let server = TestServer::new(create_app().await);

    server
        .get("/typed-header")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn typed_header_jwt_parses_a_bearer_authorization_header() {
    let server = TestServer::new(create_app().await);

    let response = server
        .get("/typed-header")
        .add_header(AUTHORIZATION, "Bearer any.opaque.value")
        .await;

    response.assert_status_ok();
    response.assert_text("ok");
}
