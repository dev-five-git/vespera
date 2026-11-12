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

//!
//! Handlers can extract the peer address with
//! [`ConnectInfo<SocketAddr>`](axum::extract::ConnectInfo): the router is
//! served via `into_make_service_with_connect_info::<SocketAddr>()`.
//!
//! [`Serve::serve_with_graceful_shutdown`] stops accepting connections when
//! a signal future resolves and waits for in-flight requests (and, with the
//! `cron` feature, stops the cron scheduler and waits a bounded time for
//! in-flight cron runs):
//!
//! ```no_run
//! use vespera::Serve;
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     vespera::axum::Router::new()
//!         .serve_with_graceful_shutdown("0.0.0.0:3000", async {
//!             // e.g. `let _ = tokio::signal::ctrl_c().await;` (tokio `signal` feature)
//!             std::future::pending::<()>().await;
//!         })
//!         .await
//! }
//! ```

use std::{future::Future, io, net::SocketAddr};

use tokio::net::ToSocketAddrs;

/// Extension trait that adds a one-liner [`Serve::serve`] method to
/// any [`axum::Router`].
pub trait Serve {
    /// Bind a TCP listener to `addr` and drive [`axum::serve()`] until
    /// the listener stops.
    ///
    /// `addr` accepts anything that implements
    /// [`tokio::net::ToSocketAddrs`] — strings (`"0.0.0.0:3000"`),
    /// tuples (`("127.0.0.1", 8080)`), [`std::net::SocketAddr`], …
    ///
    /// The peer address is available to handlers through
    /// [`axum::extract::ConnectInfo<SocketAddr>`].
    fn serve(self, addr: impl ToSocketAddrs) -> impl Future<Output = io::Result<()>>;

    /// Like [`Serve::serve`], but stops accepting new connections once
    /// `signal` resolves and returns after in-flight requests finish.
    ///
    /// With the `cron` feature it also shuts the `#[vespera::cron]`
    /// scheduler down (process-wide): no new runs start once `signal`
    /// resolves, and after the server drained, in-flight runs get up to
    /// `cron::DEFAULT_SHUTDOWN_GRACE` (10 s) to finish.
    fn serve_with_graceful_shutdown(
        self,
        addr: impl ToSocketAddrs,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> impl Future<Output = io::Result<()>>;
}

impl Serve for axum::Router {
    async fn serve(self, addr: impl ToSocketAddrs) -> io::Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            self.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }

    async fn serve_with_graceful_shutdown(
        self,
        addr: impl ToSocketAddrs,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> io::Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            self.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            signal.await;
            // No new `#[vespera::cron]` runs once shutdown has begun.
            #[cfg(feature = "cron")]
            crate::cron::shutdown();
        })
        .await?;
        // In-flight cron runs get a bounded grace period after the server
        // drained; runs still going afterwards stop with the runtime.
        #[cfg(feature = "cron")]
        let _all_finished =
            crate::cron::shutdown_and_wait(crate::cron::DEFAULT_SHUTDOWN_GRACE).await;
        Ok(())
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

    async fn serve_with_graceful_shutdown(
        self,
        addr: impl ToSocketAddrs,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> io::Result<()> {
        self.with_state(())
            .serve_with_graceful_shutdown(addr, signal)
            .await
    }
}
