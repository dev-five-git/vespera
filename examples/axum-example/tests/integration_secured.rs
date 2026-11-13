//! Security schemes + operation metadata (`routes/secured.rs`), server-sent
//! events (`routes/events.rs`) and `Serve::serve_with_graceful_shutdown`.

use axum_example::create_app;
use rstest::rstest;
use serde_json::{Value, json};
use testkit::TestServer;
use vespera::{
    Serve,
    axum::http::{
        HeaderName, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
};

async fn spec() -> Value {
    TestServer::new(create_app().await)
        .get("/openapi.json")
        .await
        .json()
}

#[rstest]
#[case::valid("Bearer demo-token", StatusCode::OK)]
#[case::wrong_token("Bearer nope", StatusCode::UNAUTHORIZED)]
#[case::not_bearer("Basic demo-token", StatusCode::UNAUTHORIZED)]
#[tokio::test]
async fn bearer_route_checks_the_token(#[case] authorization: &str, #[case] status: StatusCode) {
    let server = TestServer::new(create_app().await);
    let response = server
        .get("/secured/me")
        .add_header(AUTHORIZATION, authorization)
        .await;
    response.assert_status(status);
    if status == StatusCode::OK {
        assert_eq!(response.json::<Value>(), json!({"id": 1, "name": "demo"}));
    }
}

#[rstest]
#[case::scoped(Some("Bearer read-users-token"), StatusCode::OK)]
#[case::missing_scope(Some("Bearer demo-token"), StatusCode::FORBIDDEN)]
#[case::anonymous(None, StatusCode::UNAUTHORIZED)]
#[tokio::test]
async fn scoped_route_checks_the_scope(
    #[case] authorization: Option<&str>,
    #[case] status: StatusCode,
) {
    let server = TestServer::new(create_app().await);
    let mut request = server.get("/secured/directory");
    if let Some(authorization) = authorization {
        request = request.add_header(AUTHORIZATION, authorization);
    }
    let response = request.await;
    response.assert_status(status);
    if status == StatusCode::OK {
        assert_eq!(response.json::<Value>(), json!(["alice", "bob"]));
    }
}

#[tokio::test]
async fn api_key_route_checks_the_header() {
    let server = TestServer::new(create_app().await);
    server
        .get("/secured/reports")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let response = server
        .get("/secured/reports")
        .add_header(HeaderName::from_static("x-api-key"), "demo-key")
        .await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!(["2026-q1", "2026-q2"]));
    server.get("/secured/public").await.assert_text("public");
}

#[tokio::test]
async fn spec_documents_security_and_operation_metadata() {
    let spec = spec().await;
    assert_eq!(
        spec["components"]["securitySchemes"],
        json!({
            "apiKey": {"type": "apiKey", "name": "X-API-Key", "in": "header"},
            "bearerAuth": {"type": "http", "scheme": "bearer", "bearerFormat": "JWT"},
            "oauth2": {
                "type": "oauth2",
                "description": "Demo authorization server",
                "flows": {
                    "clientCredentials": {
                        "tokenUrl": "https://auth.example.com/token",
                        "scopes": {"read:users": "Read the user directory"}
                    },
                    "authorizationCode": {
                        "authorizationUrl": "https://auth.example.com/authorize",
                        "tokenUrl": "https://auth.example.com/token",
                        "scopes": {
                            "read:users": "Read the user directory",
                            "write:users": "Modify users"
                        }
                    }
                }
            },
            "oidc": {
                "type": "openIdConnect",
                "openIdConnectUrl": "https://auth.example.com/.well-known/openid-configuration"
            }
        })
    );

    // Scoped requirements: one alternative per tuple, scopes kept in order.
    assert_eq!(
        spec["paths"]["/secured/directory"]["get"]["security"],
        json!([{"oauth2": ["read:users"]}, {"oidc": ["openid", "read:users"]}])
    );

    let me = &spec["paths"]["/secured/me"]["get"];
    assert_eq!(me["security"], json!([{"bearerAuth": []}]));
    assert_eq!(me["operationId"], "getCurrentUser");
    assert_eq!(me["summary"], "Current user");
    assert_eq!(
        me["responses"]["200"]["content"]["application/json"]["example"],
        json!({"id": 1, "name": "demo"})
    );
    assert!(me["responses"]["401"].is_object());

    let reports = &spec["paths"]["/secured/reports"]["get"];
    assert_eq!(reports["security"], json!([{"apiKey": []}]));
    assert_eq!(reports["deprecated"], true);

    // `security = []` is kept as an explicit empty requirement list.
    assert_eq!(
        spec["paths"]["/secured/public"]["get"]["security"],
        json!([])
    );

    let tags = spec["tags"].as_array().unwrap();
    assert!(
        tags.contains(&json!({"name": "secured", "description": "Security-scheme demo routes"}))
    );
}

#[tokio::test]
async fn sse_route_streams_events() {
    let server = TestServer::new(create_app().await);
    let response = server.get("/events").await;
    response.assert_status_ok();
    assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");
    assert_eq!(
        response.text(),
        "event: tick\ndata: tick-0\n\nevent: tick\ndata: tick-1\n\nevent: tick\ndata: tick-2\n\n"
    );

    let spec = spec().await;
    assert_eq!(
        spec["paths"]["/events"]["get"]["responses"]["200"]["content"],
        json!({"text/event-stream": {"schema": {"type": "string"}}})
    );
}

/// `serve_with_graceful_shutdown` returns `Ok` once the signal resolves.
#[tokio::test]
async fn serve_with_graceful_shutdown_stops_on_signal() {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(create_app().await.serve_with_graceful_shutdown(
        "127.0.0.1:0",
        async {
            let _ = stopped.await;
        },
    ));
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
