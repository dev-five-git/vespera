//! `#[route]` accepts a `Path<T>` that reads MORE parameters than the route's
//! own `path` declares: the app may be nested under a parameterised prefix
//! (`Router::nest("/{tenant}", app)`), whose parameters axum also hands to
//! the extractor. These handlers compile and dispatch.

use serde::Deserialize;
use tower::ServiceExt;
use vespera::axum::{
    Router,
    body::{Body, to_bytes},
    extract::Path,
    http::Request,
    routing::get,
};

#[vespera::route(get, path = "/items/{id}")]
pub async fn tuple_with_prefix(Path((tenant, id)): Path<(String, u32)>) -> String {
    format!("{tenant}:{id}")
}

#[vespera::route(get, path = "/me")]
pub async fn scalar_from_prefix(Path(tenant): Path<String>) -> String {
    tenant
}

#[derive(Deserialize)]
pub struct TenantItem {
    pub tenant: String,
    pub id: u32,
}

#[vespera::route(get, path = "/things/{id}")]
pub async fn struct_field_from_prefix(Path(p): Path<TenantItem>) -> String {
    format!("{}:{}", p.tenant, p.id)
}

async fn call(app: Router, uri: &str) -> String {
    let response = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{uri}: {}",
        response.status()
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn nested_prefix_params_reach_the_extractor() {
    let inner = Router::new()
        .route("/items/{id}", get(tuple_with_prefix))
        .route("/me", get(scalar_from_prefix))
        .route("/things/{id}", get(struct_field_from_prefix));
    let app = Router::new().nest("/{tenant}", inner);

    assert_eq!(call(app.clone(), "/acme/items/7").await, "acme:7");
    assert_eq!(call(app.clone(), "/acme/me").await, "acme");
    assert_eq!(call(app, "/acme/things/9").await, "acme:9");
}
