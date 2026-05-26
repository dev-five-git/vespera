//! Admin-only routes — registered under the `"admin"` app name.
//! Reachable from Java only when the request carries the
//! `X-Vespera-App: admin` header (or the user has installed a custom
//! [`AppNameResolver`](com.devfive.vespera.bridge.AppNameResolver)
//! that resolves to `"admin"`).

use serde::Serialize;
use vespera::Schema;
use vespera::axum::Json;

/// Snapshot of admin dashboard state.
#[derive(Serialize, Schema)]
#[serde(rename_all = "camelCase")]
pub struct DashboardInfo {
    pub system: String,
    pub mode: String,
    pub active_users: u32,
    pub uptime_seconds: u64,
}

/// Admin dashboard endpoint — only reachable via the `"admin"` app.
#[allow(clippy::unused_async)]
#[vespera::route(get)]
pub async fn dashboard() -> Json<DashboardInfo> {
    Json(DashboardInfo {
        system: "rust-jni-demo".to_owned(),
        mode: "admin".to_owned(),
        active_users: 42,
        uptime_seconds: 12_345,
    })
}
