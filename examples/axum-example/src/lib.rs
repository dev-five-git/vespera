mod models;
mod routes;

use std::sync::Arc;

use sea_orm::{Database, DatabaseConnection};
use serde::{Deserialize, Serialize};
use third::ThirdApp;
use vespera::{Schema, axum, vespera};

pub struct AppState {
    pub config: String,
    pub db: Arc<DatabaseConnection>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct TestStruct {
    pub name: String,
    pub age: u32,
}

/// Create the application router for testing
pub async fn create_app() -> axum::Router {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    vespera!(
        openapi = ["examples/axum-example/openapi.json", "openapi.json"],
        docs_url = "/docs",
        redoc_url = "/redoc",
        merge = [ThirdApp]
    )
    .with_state(Arc::new(AppState {
        config: "test".to_string(),
        db: Arc::new(db),
    }))
}

/// Create the application router with a layer for testing VesperaRouter::layer
pub async fn create_app_with_layer() -> axum::Router {
    use tower_http::cors::{Any, CorsLayer};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let db = Database::connect("sqlite::memory:").await.unwrap();
    vespera!(
        openapi = ["examples/axum-example/openapi.json", "openapi.json"],
        docs_url = "/docs",
        redoc_url = "/redoc",
        merge = [ThirdApp]
    )
    .layer(cors)
    .with_state(Arc::new(AppState {
        config: "test".to_string(),
        db: Arc::new(db),
    }))
}
