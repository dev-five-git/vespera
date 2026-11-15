//! Scheduled jobs: `vespera!` discovers them by walking the crate's module
//! tree (cron discovery), on every invocation.

/// Purge expired carts.
#[vespera::cron("0 */15 * * * *")]
pub async fn purge_carts() {}

/// Rebuild the catalog search index.
#[vespera::cron("0 0 3 * * *", overlap = "skip")]
pub async fn reindex_catalog() {}

/// Jobs declared in an inline module.
pub mod reports {
    /// Send the daily sales report.
    #[vespera::cron("0 0 8 * * *")]
    pub async fn daily_sales() {}
}
