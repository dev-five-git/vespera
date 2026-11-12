//! serde attributes that change the wire shape (`with`, `into`, `try_from`,
//! container / field `#[schema(value_type)]`) and SeaORM `Json` columns: the
//! runtime wire bytes match what `openapi.json` documents.

use axum_example::create_app;
use rstest::rstest;
use serde_json::{Value, json};
use testkit::TestServer;
use vespera::axum::http::StatusCode;

fn spec() -> Value {
    serde_json::from_str(&std::fs::read_to_string("openapi.json").unwrap()).unwrap()
}

#[tokio::test]
async fn custom_codecs_round_trip_in_their_documented_shape() {
    let server = TestServer::new(create_app().await);
    let times: Value = server.get("/codecs/times").await.json();
    // `ts_seconds` / `ts_milliseconds_option` → integers, `hex_bytes` → string.
    assert_eq!(
        times,
        json!({"createdAt": 1_700_000_000, "expiresAt": 1_800_000_000_000_i64, "checksum": "deadbeef"})
    );
    let response = server.post("/codecs/times").json(&times).await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), times);
}

#[rstest]
#[case::color("/codecs/color", json!("#0a7fff"), StatusCode::OK, json!("#0a7fff"))]
#[case::money(
    "/codecs/money",
    json!({"cents": 1234, "currency": "EUR"}),
    StatusCode::OK,
    json!({"amount": "12.34", "currency": "EUR"})
)]
#[case::stamp("/codecs/stamp", json!(1_700_000_000), StatusCode::OK, json!(1_700_000_000))]
#[case::json(
    "/codecs/json",
    json!({"body": [1, "two"], "meta": null, "extra": 3}),
    StatusCode::OK,
    json!({"body": [1, "two"], "meta": null, "extra": 3})
)]
#[tokio::test]
async fn container_conversions_round_trip(
    #[case] path: &str,
    #[case] body: Value,
    #[case] status: StatusCode,
    #[case] expected: Value,
) {
    let server = TestServer::new(create_app().await);
    let response = server.post(path).json(&body).await;
    response.assert_status(status);
    assert_eq!(response.json::<Value>(), expected);
}

#[tokio::test]
async fn invalid_try_from_input_is_rejected() {
    let server = TestServer::new(create_app().await);
    let response = server.post("/codecs/color").json(&json!("red")).await;
    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn seaorm_json_columns_carry_any_json_value() {
    let server = TestServer::new(create_app().await);
    let document: Value = server.get("/codecs/document").await.json();
    assert_eq!(
        document,
        json!({"id": 1, "body": [1, "two", {"three": 3}], "meta": null})
    );
}

/// The documented component shapes, response and request views.
#[rstest]
#[case::event_times("EventTimes")]
#[case::event_times_input("EventTimesInput")]
#[case::rgb("Rgb")]
#[case::money("Money")]
#[case::money_input("MoneyInput")]
#[case::unix_seconds("UnixSeconds")]
#[case::json_payloads("JsonPayloads")]
#[case::document_response("DocumentResponse")]
fn openapi_documents_the_wire_shape(#[case] component: &str) {
    let spec = spec();
    insta::assert_snapshot!(
        format!("codecs_component_{component}"),
        serde_json::to_string_pretty(&spec["components"]["schemas"][component]).unwrap()
    );
}

#[test]
fn request_bodies_use_the_input_views() {
    let spec = spec();
    let body_ref = |path: &str| {
        spec["paths"][path]["post"]["requestBody"]["content"]["application/json"]["schema"]["$ref"]
            .clone()
    };
    assert_eq!(body_ref("/codecs/money"), "#/components/schemas/MoneyInput");
    assert_eq!(
        body_ref("/codecs/times"),
        "#/components/schemas/EventTimesInput"
    );
    assert_eq!(body_ref("/codecs/color"), "#/components/schemas/Rgb");
}
