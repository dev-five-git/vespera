//! [`Serve`] — extension trait that lets you start an `axum::Router`
//! with a one-liner.
//!
//! ```no_run
//! use vespera::Serve;
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     vespera::axum::Router::new().serve("0.0.0.0:3000").await
//! }
//! ```
//!
//! Pairs naturally with the [`vespera!`](vespera_macro::vespera) macro
//! (marked `ignore` because the macro scans the caller's `src/routes/`
//! at compile time, which doesn't exist in a doctest sandbox):
//!
//! ```ignore
//! vespera!(title = "My API").serve("0.0.0.0:3000").await
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

/// Lets a **stateless** merged app from `vespera!(merge = [...])` —
/// which returns a [`crate::VesperaRouter<()>`] rather than a plain
/// `axum::Router` — start with the same one-liner, without the user
/// having to remember the `.with_state(())` finalizer first:
///
/// ```ignore
/// vespera!(merge = [other::App]).serve("0.0.0.0:3000").await
/// ```
///
/// Finalizing with `()` runs the deferred child-router merge and layer
/// replay (see [`crate::VesperaRouter::with_state`]) before binding, so
/// merged routes and layers are present when the listener starts.
impl Serve for crate::VesperaRouter<()> {
    async fn serve(self, addr: impl ToSocketAddrs) -> io::Result<()> {
        self.with_state(()).serve(addr).await
    }
}
