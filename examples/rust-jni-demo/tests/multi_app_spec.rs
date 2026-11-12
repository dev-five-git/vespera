//! Two `vespera!` invocations in one crate (`create_app` / `admin_app`, with
//! different `dir` + `title`) must each serve THEIR OWN embedded OpenAPI
//! spec.  Regression test: both invocations used to share one embed sidecar
//! (`target/vespera/vespera_spec-<crate>.json`), so one app served the
//! other's document.

use rust_jni_demo::{admin_app, create_app};
use serde_json::Value;
use testkit::TestServer;

async fn served_spec(router: vespera::axum::Router) -> Value {
    let response = TestServer::new(router).get("/openapi.json").await;
    response.assert_status_ok();
    response.json()
}

#[tokio::test]
async fn each_app_serves_its_own_spec() {
    let default_spec = served_spec(create_app()).await;
    let admin_spec = served_spec(admin_app()).await;

    assert_eq!(default_spec["info"]["title"], "Document Validation API");
    assert_eq!(admin_spec["info"]["title"], "Admin Dashboard API");
    assert!(default_spec["paths"].get("/health").is_some());
    assert!(admin_spec["paths"].get("/health").is_none());
}
