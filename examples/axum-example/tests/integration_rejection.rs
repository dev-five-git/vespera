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
}

#[tokio::test]
async fn result_query_dispatches_and_hands_the_rejection_to_the_handler() {
    let server = TestServer::new(create_app().await);

    server
        .get("/rejection?q=a&limit=3")
        .await
        .assert_text("a:3");
    server.get("/rejection").await.assert_text("invalid query");
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
