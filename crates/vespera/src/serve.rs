//! [`Serve`] — extension trait that lets you start an `axum::Router`
//! with a one-liner.
//!
//! ```no_run
//! use vespera::{vespera, Serve};
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     vespera!(title = "My API").serve("0.0.0.0:3000").await
//! }
//! ```
//!
//! Equivalent to:
//!
//! ```ignore
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
//! axum::serve(listener, app).await?;
//! ```

use std::io;

use tokio::net::ToSocketAddrs;

/// Extension trait that adds a one-liner [`Serve::serve`] method to
/// any [`axum::Router`].
pub trait Serve {
    /// Bind a TCP listener to `addr` and drive [`axum::serve`] until
    /// the listener stops.
    ///
    /// `addr` accepts anything that implements
    /// [`tokio::net::ToSocketAddrs`] — strings (`"0.0.0.0:3000"`),
    /// tuples (`("127.0.0.1", 8080)`), [`std::net::SocketAddr`], …
    fn serve(self, addr: impl ToSocketAddrs) -> impl std::future::Future<Output = io::Result<()>>;
}

impl Serve for axum::Router {
    async fn serve(self, addr: impl ToSocketAddrs) -> io::Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, self).await
    }
}
