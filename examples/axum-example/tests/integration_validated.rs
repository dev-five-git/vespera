//! `Validated` after a `Path` extractor, `ValidatedWith` with a context from
//! the router state, and the string-serialized std / `url` builtins — runtime
//! behaviour plus the documented OpenAPI shape.

use axum_example::create_app;
use rstest::rstest;
use serde_json::{Value, json};
use testkit::TestServer;
use vespera::axum::http::StatusCode;

fn valid_user() -> Value {
    json!({"username": "alice_1", "email": "alice@example.com", "age": 30, "tags": ["a"]})
}

#[tokio::test]
async fn path_then_validated_body_accepts_valid_payload() {
    let server = TestServer::new(create_app().await);
    let response = server.put("/validated/users/7").json(&valid_user()).await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), valid_user());
}

#[tokio::test]
async fn path_then_validated_body_rejects_invalid_payload_with_422() {
    let server = TestServer::new(create_app().await);
    let mut body = valid_user();
    body["username"] = json!("x");
    let response = server.put("/validated/users/7").json(&body).await;
    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    let errors: Value = response.json();
    assert_eq!(errors["errors"][0]["path"], "username");
}

#[rstest]
#[case::policy_ok(json!({"title": "t", "tags": ["app-one", "app-two"]}), StatusCode::OK, None)]
#[case::policy_violation(json!({"title": "t", "tags": ["app-one", "other"]}), StatusCode::UNPROCESSABLE_ENTITY, Some("tags"))]
#[case::plain_rule_violation(json!({"title": "", "tags": []}), StatusCode::UNPROCESSABLE_ENTITY, Some("title"))]
#[tokio::test]
async fn validated_with_uses_state_context(
    #[case] body: Value,
    #[case] status: StatusCode,
    #[case] error_path: Option<&str>,
) {
    let server = TestServer::new(create_app().await);
    let response = server.post("/validated/tags").json(&body).await;
    response.assert_status(status);
    let json: Value = response.json();
    match error_path {
        Some(path) => assert_eq!(json["errors"][0]["path"], path),
        None => assert_eq!(json, body),
    }
}

#[tokio::test]
async fn std_string_types_round_trip_as_strings() {
    let server = TestServer::new(create_app().await);
    let endpoint: Value = server.get("/network").await.json();
    insta::assert_snapshot!("network_endpoint_wire", endpoint.to_string());

    let response = server.post("/network").json(&endpoint).await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), endpoint);
}

#[test]
fn openapi_documents_new_validated_routes_and_builtins() {
    let spec: Value =
        serde_json::from_str(&std::fs::read_to_string("openapi.json").unwrap()).unwrap();
    for (path, method) in [
        ("/validated/users/{id}", "put"),
        ("/validated/tags", "post"),
    ] {
        let operation = &spec["paths"][path][method];
        assert!(
            operation["responses"]["422"].is_object(),
            "{method} {path} documents 422"
        );
        assert!(
            operation["requestBody"].is_object(),
            "{method} {path} documents its body"
        );
    }
    let props = &spec["components"]["schemas"]["NetworkEndpoint"]["properties"];
    for (field, format) in [
        ("ip", None),
        ("ipv4", Some("ipv4")),
        ("ipv6", Some("ipv6")),
        ("bind", None),
        ("data_dir", None),
        ("homepage", Some("uri")),
        ("callback", Some("uri")),
    ] {
        assert_eq!(props[field]["format"].as_str(), format, "{field} format");
        assert!(
            props[field]["type"].to_string().contains("string"),
            "{field} is a string"
        );
    }
}

#[rstest]
#[case::valid("/validated/search?q=rust&limit=10", StatusCode::OK, None)]
#[case::short_term(
    "/validated/search?q=r&limit=10",
    StatusCode::UNPROCESSABLE_ENTITY,
    Some("q")
)]
#[case::limit_too_big(
    "/validated/search?q=rust&limit=101",
    StatusCode::UNPROCESSABLE_ENTITY,
    Some("limit")
)]
#[case::page_ok("/validated/pages/3", StatusCode::OK, None)]
#[case::page_zero("/validated/pages/0", StatusCode::UNPROCESSABLE_ENTITY, Some("page"))]
#[tokio::test]
async fn validated_query_and_path(
    #[case] uri: &str,
    #[case] status: StatusCode,
    #[case] error_path: Option<&str>,
) {
    let server = TestServer::new(create_app().await);
    let response = server.get(uri).await;
    response.assert_status(status);
    if let Some(path) = error_path {
        assert_eq!(response.json::<Value>()["errors"][0]["path"], path);
    }
}

#[rstest]
#[case::valid("username=alice&email=alice%40example.com", StatusCode::OK, None)]
#[case::bad_email(
    "username=alice&email=nope",
    StatusCode::UNPROCESSABLE_ENTITY,
    Some("email")
)]
#[case::short_name(
    "username=al&email=alice%40example.com",
    StatusCode::UNPROCESSABLE_ENTITY,
    Some("username")
)]
#[tokio::test]
async fn validated_form(
    #[case] body: &'static str,
    #[case] status: StatusCode,
    #[case] error_path: Option<&str>,
) {
    let server = TestServer::new(create_app().await);
    let response = server
        .post("/validated/signup")
        .content_type("application/x-www-form-urlencoded")
        .bytes(body.into())
        .await;
    response.assert_status(status);
    let json: Value = response.json();
    match error_path {
        Some(path) => assert_eq!(json["errors"][0]["path"], path),
        None => assert_eq!(
            json,
            json!({"username": "alice", "email": "alice@example.com"})
        ),
    }
}

fn valid_profile() -> Value {
    json!({
        "display_name": "Al",
        "email": "al@example.com",
        "homepage": null,
        "age": 18,
        "hashtags": ["#rust"],
        "last_ip": "::1",
        "country": "KR",
        "avatar": "me.png",
        "nickname": "al1"
    })
}

fn garde_profile_property(field: &str) -> Value {
    let spec: Value =
        serde_json::from_str(&std::fs::read_to_string("openapi.json").unwrap()).unwrap();
    spec["components"]["schemas"]["GardeProfile"]["properties"][field].clone()
}

/// `#[garde(...)]` rules are documented in `openapi.json`, and the documented
/// bound is exactly where the `422` starts: the boundary value passes, one
/// step past it fails on that field.
#[rstest]
#[case::age_min("age", "minimum", json!(18), json!(17))]
#[case::age_max("age", "maximum", json!(120), json!(121))]
#[case::display_name_min("display_name", "minLength", json!("Al"), json!("A"))]
#[case::display_name_max_chars("display_name", "maxLength", json!("é".repeat(20)), json!("é".repeat(21)))]
#[case::hashtags_max_items("hashtags", "maxItems", json!(["#a", "#b", "#c"]), json!(["#a", "#b", "#c", "#d"]))]
#[tokio::test]
async fn documented_garde_bounds_match_422_boundary(
    #[case] field: &str,
    #[case] keyword: &str,
    #[case] at_bound: Value,
    #[case] past_bound: Value,
) {
    let documented = garde_profile_property(field)[keyword].as_u64().unwrap();
    let measure = |value: &Value| match value {
        Value::Number(n) => n.as_u64().unwrap(),
        Value::String(s) => s.chars().count() as u64,
        Value::Array(items) => items.len() as u64,
        other => panic!("unexpected {other}"),
    };
    assert_eq!(measure(&at_bound), documented, "{field}.{keyword}");

    let server = TestServer::new(create_app().await);
    let mut body = valid_profile();
    body[field] = at_bound;
    server
        .post("/validated/profiles")
        .json(&body)
        .await
        .assert_status_ok();

    body[field] = past_bound;
    let response = server.post("/validated/profiles").json(&body).await;
    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.json::<Value>()["errors"][0]["path"], field);
}

/// Pattern / format rules: the documented `items.pattern` and `format`
/// reject what the runtime rejects.
#[rstest]
#[case::hashtag_prefix("hashtags", json!(["rust"]), "hashtags[0]")]
#[case::email_format("email", json!("not-an-email"), "email")]
#[case::country_pattern("country", json!("kr"), "country")]
#[case::avatar_suffix("avatar", json!("me.jpg"), "avatar")]
#[tokio::test]
async fn documented_garde_patterns_match_422(
    #[case] field: &str,
    #[case] value: Value,
    #[case] error_path: &str,
) {
    let property = garde_profile_property(field);
    let documented = [
        &property["pattern"],
        &property["format"],
        &property["items"]["pattern"],
    ];
    assert!(
        documented.iter().any(|v| v.is_string()),
        "{field} documents a pattern / format: {property}"
    );

    let server = TestServer::new(create_app().await);
    let mut body = valid_profile();
    body[field] = value;
    let response = server.post("/validated/profiles").json(&body).await;
    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.json::<Value>()["errors"][0]["path"], error_path);
}
