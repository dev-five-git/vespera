/// Health check endpoint
#[allow(clippy::unused_async)]
#[vespera::route(get)]
pub async fn health() -> &'static str {
    "ok"
}
