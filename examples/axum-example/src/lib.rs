mod routes;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vespera::{Schema, axum, vespera};

pub struct AppState {
    pub config: String,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct TestStruct {
    pub name: String,
    pub age: u32,
}

/// Create the application router for testing
pub fn create_app() -> axum::Router {
    vespera!(
        openapi = ["examples/axum-example/openapi.json", "openapi.json"],
        docs_url = "/docs",
        redoc_url = "/redoc"
    )
    .with_state(Arc::new(AppState {
        config: "test".to_string(),
    }))
}
