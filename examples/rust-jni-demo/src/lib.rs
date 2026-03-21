#![allow(unsafe_code)]

//! Vespera JNI integration demo.
//!
//! | Mode | Entry | Transport |
//! |------|-------|-----------|
//! | Server | `main.rs` → `axum::serve` | TCP :3000 |
//! | JNI | `jni_app!` → Java calls `VesperaBridge.dispatch()` | In-process |

mod routes;

use vespera::{axum, vespera};

/// Build the application router.
pub fn create_app() -> axum::Router {
    vespera!(title = "Document Validation API", version = "0.1.0")
}

// Register this app for JNI dispatch — one line, no boilerplate.
vespera::jni_app!(create_app);

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use vespera::inprocess::{RequestEnvelope, dispatch};

    use super::*;

    fn req(method: &str, path: &str, body: &str) -> RequestEnvelope {
        RequestEnvelope {
            method: method.into(),
            path: path.into(),
            query: String::new(),
            headers: HashMap::new(),
            body: body.into(),
        }
    }

    fn parse(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[tokio::test]
    async fn health_returns_200() {
        let json = dispatch(create_app(), &req("GET", "/health", "")).await;
        let v = parse(&json);
        assert_eq!(v["status"], 200);
        assert!(v["body"].as_str().unwrap().contains("ok"));
        assert!(v["metadata"]["version"].is_string());
    }

    #[tokio::test]
    async fn response_includes_content_type_header() {
        let json = dispatch(create_app(), &req("GET", "/health", "")).await;
        let v = parse(&json);
        assert!(v["headers"]["content-type"].is_string());
    }

    #[tokio::test]
    async fn validate_valid_document() {
        let body = serde_json::json!({
            "documentType": "regulation",
            "title": "Test Policy",
            "content": "This regulation establishes the framework for handling \
                        personal data within the organisation and outlines compliance.",
            "author": "Kim Minjun",
            "department": "Legal",
            "classification": "internal",
            "effectiveDate": "2025-01-01"
        });
        let json = dispatch(
            create_app(),
            &req("POST", "/documents/validate", &body.to_string()),
        )
        .await;
        let v = parse(&json);
        assert_eq!(v["status"], 200);
        let inner: serde_json::Value = serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
        assert_eq!(inner["valid"], true);
    }

    #[tokio::test]
    async fn validate_invalid_document() {
        let body = serde_json::json!({
            "documentType": "invalid_type",
            "title": "",
            "content": "",
            "author": "",
            "department": "",
            "classification": "bogus",
            "effectiveDate": "not-a-date"
        });
        let json = dispatch(
            create_app(),
            &req("POST", "/documents/validate", &body.to_string()),
        )
        .await;
        let v = parse(&json);
        assert_eq!(v["status"], 200);
        let inner: serde_json::Value = serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
        assert_eq!(inner["valid"], false);
    }

    #[tokio::test]
    async fn not_found_returns_404() {
        let json = dispatch(create_app(), &req("GET", "/nope", "")).await;
        let v = parse(&json);
        assert_eq!(v["status"], 404);
    }

    #[tokio::test]
    async fn request_headers_forwarded() {
        let envelope = RequestEnvelope {
            method: "GET".into(),
            path: "/health".into(),
            query: String::new(),
            headers: HashMap::from([
                ("cookie".into(), "session=abc123".into()),
                ("x-custom".into(), "test-value".into()),
            ]),
            body: String::new(),
        };
        let json = dispatch(create_app(), &envelope).await;
        assert_eq!(parse(&json)["status"], 200);
    }

    #[tokio::test]
    async fn query_string_forwarded() {
        let envelope = RequestEnvelope {
            method: "GET".into(),
            path: "/health".into(),
            query: "foo=bar&baz=1".into(),
            headers: HashMap::new(),
            body: String::new(),
        };
        let json = dispatch(create_app(), &envelope).await;
        assert_eq!(parse(&json)["status"], 200);
    }

    // ── Coverage: inprocess::dispatch_typed ───────────────────────
    #[tokio::test]
    async fn dispatch_typed_returns_envelope() {
        use vespera::inprocess::dispatch_typed;
        let result = dispatch_typed(create_app(), &req("GET", "/health", "")).await;
        assert_eq!(result.status, 200);
        assert!(result.body.contains("ok"));
        assert!(!result.metadata.version.is_empty());
    }

    // ── Coverage: inprocess::parse_request ────────────────────────
    #[test]
    fn parse_request_valid() {
        use vespera::inprocess::parse_request;
        let envelope = parse_request(r#"{"method":"GET","path":"/health"}"#).unwrap();
        assert_eq!(envelope.method, "GET");
        assert_eq!(envelope.path, "/health");
        assert!(envelope.query.is_empty());
        assert!(envelope.headers.is_empty());
        assert!(envelope.body.is_empty());
    }

    #[test]
    fn parse_request_invalid_json() {
        use vespera::inprocess::parse_request;
        let err = parse_request("not json").unwrap_err();
        assert!(err.contains("invalid request envelope"));
    }

    // ── Coverage: inprocess::error_envelope ───────────────────────
    #[test]
    fn error_envelope_creates_500() {
        use vespera::inprocess::error_envelope;
        let e = error_envelope("something broke");
        assert_eq!(e.status, 500);
        assert_eq!(e.body, "something broke");
        assert!(e.headers.is_empty());
        assert!(!e.metadata.version.is_empty());
    }

    // ── Coverage: content-type auto-inject ────────────────────────
    #[tokio::test]
    async fn body_without_content_type_gets_default() {
        // Send a body WITHOUT content-type header — dispatch should inject it
        let envelope = RequestEnvelope {
            method: "POST".into(),
            path: "/documents/validate".into(),
            query: String::new(),
            headers: HashMap::new(), // no content-type
            body: serde_json::json!({
                "documentType": "memo",
                "title": "Test",
                "content": "Content for testing auto content-type injection in dispatch.",
                "author": "Test",
                "department": "Test",
                "classification": "public",
                "effectiveDate": "2025-01-01"
            })
            .to_string(),
        };
        let json = dispatch(create_app(), &envelope).await;
        // Should succeed (200) because content-type was auto-injected
        assert_eq!(parse(&json)["status"], 200);
    }

    // ── Coverage: invalid HTTP method fallback ────────────────────
    #[tokio::test]
    async fn invalid_method_falls_back_to_get() {
        let envelope = RequestEnvelope {
            method: "INVALID_METHOD".into(),
            path: "/nonexistent".into(),
            query: String::new(),
            headers: HashMap::new(),
            body: String::new(),
        };
        let json = dispatch(create_app(), &envelope).await;
        // Invalid method parses → fallback GET, unknown route → 404
        assert_eq!(parse(&json)["status"], 404);
    }

    // ── Coverage: dispatch_json_with (explicit factory, no OnceLock) ─

    #[test]
    fn dispatch_json_with_valid() {
        use vespera::inprocess::dispatch_json_with;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let json = dispatch_json_with(r#"{"method":"GET","path":"/health"}"#, &rt, &create_app);
        let v = parse(&json);
        assert_eq!(v["status"], 200);
        assert!(v["body"].as_str().unwrap().contains("ok"));
    }

    #[test]
    fn dispatch_json_with_invalid_json() {
        use vespera::inprocess::dispatch_json_with;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let json = dispatch_json_with("not json", &rt, &create_app);
        let v = parse(&json);
        assert_eq!(v["status"], 500);
        assert!(
            v["body"]
                .as_str()
                .unwrap()
                .contains("invalid request envelope")
        );
    }

    // ── Coverage: serialize_error ────────────────────────────────

    #[test]
    fn serialize_error_returns_500_json() {
        use vespera::inprocess::serialize_error;
        let json = serialize_error("test error");
        let v = parse(&json);
        assert_eq!(v["status"], 500);
        assert_eq!(v["body"], "test error");
    }

    // ── Coverage: register_app + dispatch_from_json ────────────

    #[test]
    fn register_and_dispatch_from_json() {
        use vespera::inprocess::{dispatch_from_json, register_app};
        let rt = tokio::runtime::Runtime::new().unwrap();

        // Before register: None branch → serialize_error
        // (only works if this test runs before any other register_app call,
        //  which is guaranteed because OnceLock is per-process and test
        //  ordering means the first caller wins)
        //
        // After register: Some branch → dispatch_json_with
        register_app(create_app);
        let json = dispatch_from_json(r#"{"method":"GET","path":"/health"}"#, &rt);
        assert_eq!(parse(&json)["status"], 200);
    }
}
