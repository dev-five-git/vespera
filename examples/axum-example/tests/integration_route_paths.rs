//! axum 0.7 route notation (`:id`, `*rest`) is accepted by `#[route]` and
//! normalized to axum 0.8 syntax: the route dispatches (axum 0.8 would panic
//! at router build on the raw `:id`) and OpenAPI documents `{id}` / `{rest}`.

use axum_example::create_app;
use testkit::TestServer;

#[tokio::test]
async fn axum_07_notation_dispatches() {
    let server = TestServer::new(create_app().await);

    let response = server.get("/path/legacy/7/files/a/b.txt").await;
    response.assert_status_ok();
    assert_eq!(response.text(), "7:a/b.txt");
}

/// `/report-{year}`: a parameter after a static prefix in its segment routes
/// and is documented (the template keeps the prefix).
#[tokio::test]
async fn prefixed_segment_parameter_dispatches_and_is_documented() {
    let server = TestServer::new(create_app().await);

    server
        .get("/path/report-2026")
        .await
        .assert_text("report 2026");

    let spec: serde_json::Value = server.get("/openapi.json").await.json();
    let params = &spec["paths"]["/path/report-{year}"]["get"]["parameters"];
    assert_eq!(params[0]["name"], "year");
    assert_eq!(params[0]["in"], "path");
    assert_eq!(params[0]["schema"]["format"], "uint16");
    assert_eq!(params.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn axum_07_notation_is_documented_as_openapi_template() {
    let server = TestServer::new(create_app().await);

    let spec: serde_json::Value = server.get("/openapi.json").await.json();
    let paths = spec["paths"].as_object().unwrap();
    assert!(
        !paths
            .keys()
            .any(|key| key.contains("/:") || key.contains("/*")),
        "no raw axum 0.7 segments in OpenAPI paths"
    );
    let op = &spec["paths"]["/path/legacy/{id}/files/{rest}"]["get"];
    let params: Vec<(&str, &str)> = op["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["name"].as_str().unwrap(), p["in"].as_str().unwrap()))
        .collect();
    assert_eq!(params, [("id", "path"), ("rest", "path")]);
    assert_eq!(
        op["parameters"][0]["schema"]["type"],
        serde_json::json!("integer")
    );
}
