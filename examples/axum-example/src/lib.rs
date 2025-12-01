mod routes;

use std::sync::Arc;

use vespera::{axum, vespera};

/// Create the application router for testing
pub fn create_app() -> axum::Router {
    vespera!().with_state(Arc::new(AppState {
        config: "test".to_string(),
    }))
}

pub struct AppState {
    pub config: String,
}
