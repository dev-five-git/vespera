// Route handlers are `pub async fn` by contract (vespera! discovers them), even when
// they never await, and the example favours readable over pedantic formatting.
#![allow(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    clippy::uninlined_format_args,
    clippy::default_trait_access
)]

mod cron;
mod models;
mod routes;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use sea_orm::{Database, DatabaseConnection};
use serde::{Deserialize, Serialize};
use third::ThirdApp;
use vespera::{Schema, axum, vespera};

pub struct AppState {
    pub config: String,
    pub db: Arc<DatabaseConnection>,
    /// Validation context borrowed by `ValidatedWith<TagPolicy, _>` routes.
    pub tag_policy: TagPolicy,
}

/// Runtime validation policy for `routes/validated.rs` — the context of the
/// `ValidatedWith<TagPolicy, _>` extractor.
pub struct TagPolicy {
    /// Every tag must start with this prefix.
    pub required_prefix: String,
}

/// `ValidatedWith<TagPolicy, _>` borrows its context from the router state.
impl vespera::ValidationContext<TagPolicy> for Arc<AppState> {
    fn validation_context(&self) -> &TagPolicy {
        &self.tag_policy
    }
}

fn app_state(db: DatabaseConnection) -> Arc<AppState> {
    Arc::new(AppState {
        config: "test".to_string(),
        db: Arc::new(db),
        tag_policy: TagPolicy {
            required_prefix: "app-".to_string(),
        },
    })
}

/// Runs of the cron discovery probes — `[crate root, inline module, after
/// vespera!]` (asserted by `tests/integration_cron.rs`).
#[doc(hidden)]
pub static CRON_PROBE_RUNS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];

/// A cron job at the crate root is scheduled as `crate::root_probe` (not
/// `crate::lib::root_probe`).
#[vespera::cron("* * * * * *")]
pub async fn root_probe() {
    CRON_PROBE_RUNS[0].fetch_add(1, Ordering::SeqCst);
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
        openapi = "openapi.json",
        docs_url = "/docs",
        redoc_url = "/redoc",
        spec_url = "/openapi.json",
        description = "Vespera axum example API",
        summary = "Example routes exercising every vespera feature",
        terms_of_service = "https://example.com/terms",
        contact_name = "Vespera Team",
        contact_url = "https://github.com/dev-five-git/vespera",
        contact_email = "vespera@example.com",
        license = "Apache-2.0",
        // Declared once here, referenced per route via
        // `#[route(security = [...])]` (see `routes/secured.rs`).
        security_schemes = [
            { name = "bearerAuth", type = "http", scheme = "bearer", bearer_format = "JWT" },
            { name = "apiKey", type = "apiKey", in = "header", header_name = "X-API-Key" },
            {
                name = "oauth2",
                type = "oauth2",
                description = "Demo authorization server",
                flows = {
                    authorization_code = {
                        authorization_url = "https://auth.example.com/authorize",
                        token_url = "https://auth.example.com/token",
                        scopes = { "read:users" = "Read the user directory", "write:users" = "Modify users" },
                    },
                    client_credentials = {
                        token_url = "https://auth.example.com/token",
                        scopes = { "read:users" = "Read the user directory" },
                    },
                },
            },
            {
                name = "oidc",
                type = "openIdConnect",
                open_id_connect_url = "https://auth.example.com/.well-known/openid-configuration"
            }
        ],
        // Top-level tag descriptions (`tags[]` in the document).
        tags = [
            { name = "secured", description = "Security-scheme demo routes" },
            { name = "events", description = "Server-sent events" }
        ],
        merge = [ThirdApp]
    )
    .with_state(app_state(db))
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
        openapi = "openapi.json",
        docs_url = "/docs",
        redoc_url = "/redoc",
        spec_url = "/openapi.json",
        description = "Vespera axum example API",
        summary = "Example routes exercising every vespera feature",
        terms_of_service = "https://example.com/terms",
        contact_name = "Vespera Team",
        contact_url = "https://github.com/dev-five-git/vespera",
        contact_email = "vespera@example.com",
        license = "Apache-2.0",
        security_schemes = [
            { name = "bearerAuth", type = "http", scheme = "bearer", bearer_format = "JWT" },
            { name = "apiKey", type = "apiKey", in = "header", header_name = "X-API-Key" },
            {
                name = "oauth2",
                type = "oauth2",
                description = "Demo authorization server",
                flows = {
                    authorization_code = {
                        authorization_url = "https://auth.example.com/authorize",
                        token_url = "https://auth.example.com/token",
                        scopes = { "read:users" = "Read the user directory", "write:users" = "Modify users" },
                    },
                    client_credentials = {
                        token_url = "https://auth.example.com/token",
                        scopes = { "read:users" = "Read the user directory" },
                    },
                },
            },
            {
                name = "oidc",
                type = "openIdConnect",
                open_id_connect_url = "https://auth.example.com/.well-known/openid-configuration"
            }
        ],
        tags = [
            { name = "secured", description = "Security-scheme demo routes" },
            { name = "events", description = "Server-sent events" }
        ],
        merge = [ThirdApp]
    )
    .layer(cors)
    .with_state(app_state(db))
}

/// Declared after the `vespera!` calls in a file whose earlier job is
/// already registered: still discovered by the module-tree scan.
#[vespera::cron("* * * * * *")]
pub async fn after_vespera_probe() {
    CRON_PROBE_RUNS[2].fetch_add(1, Ordering::SeqCst);
}
