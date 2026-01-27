mod models;
mod routes;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use third::ThirdApp;
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
        redoc_url = "/redoc",
        merge = [ThirdApp]
    )
    .with_state(Arc::new(AppState {
        config: "test".to_string(),
    }))
}

/// Create the application router with a layer for testing VesperaRouter::layer
pub fn create_app_with_layer() -> axum::Router {
    use tower_http::cors::{Any, CorsLayer};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    vespera!(
        openapi = ["examples/axum-example/openapi.json", "openapi.json"],
        docs_url = "/docs",
        redoc_url = "/redoc",
        merge = [ThirdApp]
    )
    .layer(cors)
    .with_state(Arc::new(AppState {
        config: "test".to_string(),
    }))
}
