#![allow(unsafe_code)]

//! Vespera JNI integration demo — **multi-app** edition.
//!
//! | Mode | Entry | Transport |
//! |------|-------|-----------|
//! | Server (default app only) | `main.rs` → `axum::serve` | TCP :3000 |
//! | JNI multi-app | `jni_apps!` → Java picks app via `X-Vespera-App` header | In-process binary wire |
//!
//! Two named apps are registered:
//!
//! - **`_default`** (default — reachable without header) — the
//!   document-validation API in `src/routes/`.
//! - **`admin`** (reachable with `X-Vespera-App: admin`) — the
//!   admin dashboard API in `src/admin_routes/`.
//!
//! Both apps share the same Rust process / same Tokio runtime / same
//! JNI bridge — Spring picks the target per request based on the
//! configured `AppNameResolver` (a Java-side bean in `vespera-bridge`).

mod admin_routes;
mod routes;

use vespera::{axum, vespera};

/// Default app router (registered under `"_default"`).  Generates
/// `openapi.json` with the document-validation routes (`/health`,
/// `/echo`, `/documents/validate`).
pub fn create_app() -> axum::Router {
    vespera!(
        openapi = "openapi.json",
        title = "Document Validation API",
        version = "0.1.0"
    )
}

/// Admin app router (registered under `"admin"`).  Generates a
/// separate `openapi-admin.json` so the admin surface has its own
/// OpenAPI document — multi-app callers fetch the spec for whichever
/// app they target.
pub fn admin_app() -> axum::Router {
    vespera!(
        dir = "admin_routes",
        openapi = "openapi-admin.json",
        title = "Admin Dashboard API",
        version = "0.1.0"
    )
}

// Register both apps under their respective names.  Java side
// selects per request via the `X-Vespera-App` header — default app
// when the header is absent, `"admin"` app when the header reads
// `admin`.  Exactly one `JNI_OnLoad` is generated for the cdylib.
vespera::jni_apps! {
    "_default" => create_app,
    "admin"    => admin_app,
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use vespera::inprocess::{RequestEnvelope, dispatch};

    use super::*;

    fn req(method: &str, path: &str, body: &str) -> RequestEnvelope {
        RequestEnvelope {
            principal: None,
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
    async fn register_route_validates_with_422() {
        let ok = parse(
            &dispatch(
                create_app(),
                &req(
                    "POST",
                    "/documents/register",
                    r#"{"title":"Policy","pageCount":3}"#,
                ),
            )
            .await,
        );
        assert_eq!(ok["status"], 200, "{ok}");
        let bad = parse(
            &dispatch(
                create_app(),
                &req(
                    "POST",
                    "/documents/register",
                    r#"{"title":"","pageCount":0}"#,
                ),
            )
            .await,
        );
        assert_eq!(bad["status"], 422, "{bad}");
        assert!(bad["body"].as_str().unwrap().contains("errors"));
    }

    #[tokio::test]
    async fn diag_routes_count_finished_runs() {
        let app = create_app();
        let body = |path: &str| {
            let app = app.clone();
            let path = path.to_owned();
            async move {
                let v = parse(&dispatch(app, &req("GET", &path, "")).await);
                assert_eq!(v["status"], 200, "{path}: {v}");
                v["body"].as_str().unwrap().to_owned()
            }
        };
        assert_eq!(body("/diag/runs/rust-test").await, "0");
        assert_eq!(body("/diag/blob/rust-test/5").await, "xxxxx");
        assert_eq!(body("/diag/sleep/rust-test/1").await, "slept");
        assert_eq!(body("/diag/runs/rust-test").await, "2");
        // Sizes are clamped.
        let big = body(&format!("/diag/blob/rust-big/{}", usize::MAX)).await;
        assert_eq!(big.len(), routes::diag::MAX_BLOB_BYTES);
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
            principal: None,
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
            principal: None,
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
            principal: None,
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
            principal: None,
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

    // ── Form-urlencoded + header-inspection routes ────────────────
    fn with_headers(
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> RequestEnvelope {
        RequestEnvelope {
            principal: None,
            method: method.into(),
            path: path.into(),
            query: String::new(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: body.into(),
        }
    }

    #[tokio::test]
    async fn form_routes_echo_urlencoded_fields() {
        let form = [("content-type", "application/x-www-form-urlencoded")];
        for method in ["PATCH", "POST"] {
            let json = dispatch(
                create_app(),
                &with_headers(method, "/form", &form, "name=Jane+Doe&count=3"),
            )
            .await;
            let v = parse(&json);
            assert_eq!(v["status"], 200, "{method}: {json}");
            let inner = parse(v["body"].as_str().unwrap());
            assert_eq!(
                inner,
                serde_json::json!({"name": "Jane Doe", "count": 3, "note": null})
            );
        }
    }

    #[tokio::test]
    async fn headers_route_lists_received_header_names() {
        let json = dispatch(
            create_app(),
            &with_headers("GET", "/headers", &[("x-b", "1"), ("x-a", "2")], ""),
        )
        .await;
        let v = parse(&json);
        assert_eq!(
            parse(v["body"].as_str().unwrap()),
            serde_json::json!(["x-a", "x-b"])
        );
    }

    /// The servlet's Latin-1 view of the UTF-8 bytes `résumé` maps back to
    /// those bytes in Rust and out again as Latin-1 chars.
    #[tokio::test]
    async fn raw_header_route_round_trips_latin1_bytes() {
        let servlet_view = "r\u{c3}\u{a9}sum\u{c3}\u{a9}.pdf";
        let json = dispatch(
            create_app(),
            &with_headers("GET", "/headers/raw", &[("x-name", servlet_view)], ""),
        )
        .await;
        let v = parse(&json);
        assert_eq!(v["body"], "résumé.pdf");
        assert_eq!(
            v["headers"]["content-disposition"],
            format!("attachment; filename=\"{servlet_view}\"")
        );

        let missing = parse(&dispatch(create_app(), &req("GET", "/headers/raw", "")).await);
        assert_eq!(
            missing["headers"]["content-disposition"],
            "attachment; filename=\"\""
        );
    }

    /// `remote_addr` / `scheme` from the wire header reach the handler as
    /// `ConnectInfo` and the `Scheme` extension (scheme is optional).
    #[tokio::test]
    async fn client_route_reads_the_forwarded_connection() {
        vespera::inprocess::register_app_named("client-test", create_app);
        let call = |fields: &str| {
            let header = format!(
                r#"{{"v":1,"method":"GET","path":"/headers/client","app":"client-test"{fields}}}"#
            );
            let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
            wire.extend_from_slice(header.as_bytes());
            wire
        };
        let body = |wire: Vec<u8>| {
            let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
            String::from_utf8(wire[4 + len..].to_vec()).unwrap()
        };
        let https = vespera::inprocess::dispatch_from_bytes_async(call(
            r#","remote_addr":"[::1]:4321","scheme":"https""#,
        ))
        .await;
        assert_eq!(body(https), "https [::1]:4321");
        let plain =
            vespera::inprocess::dispatch_from_bytes_async(call(r#","remote_addr":"10.0.0.1:80""#))
                .await;
        assert_eq!(body(plain), "none 10.0.0.1:80");
    }

    /// The trusted `principal` reaches `/headers/principal`; anonymous requests get `401`.
    #[tokio::test]
    async fn principal_route_reads_the_trusted_principal() {
        let mut envelope = req("GET", "/headers/principal", "");
        envelope.principal = Some(vespera::inprocess::Principal {
            name: "alice".to_owned(),
            authorities: vec!["ROLE_USER".to_owned(), "ROLE_ADMIN".to_owned()],
            attributes: [("tenant".to_owned(), "acme".to_owned())].into(),
        });
        let v = parse(&dispatch(create_app(), &envelope).await);
        assert_eq!(v["status"], 200);
        assert_eq!(v["body"], "alice [ROLE_USER,ROLE_ADMIN] tenant=acme");

        let anonymous = parse(&dispatch(create_app(), &req("GET", "/headers/principal", "")).await);
        assert_eq!(anonymous["status"], 401);
    }
}
