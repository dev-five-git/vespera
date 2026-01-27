//! Vespera - OpenAPI generation for Rust web frameworks
//!
//! This crate provides macros and utilities for generating OpenAPI documentation
//! from your route definitions.

// Re-export vespera_core types so users don't need to depend on vespera_core directly
pub mod schema {
    pub use vespera_core::schema::*;
}

pub mod route {
    pub use vespera_core::route::*;
}

pub mod openapi {
    pub use vespera_core::openapi::*;
}

// Re-export OpenApi directly for convenience (used by merge feature)
pub use vespera_core::openapi::OpenApi;

// Re-export macros from vespera_macro
pub use vespera_macro::{Schema, export_app, route, schema, schema_type, vespera};

// Re-export serde_json for merge feature (runtime spec merging)
pub use serde_json;

// Re-export axum for convenience
pub mod axum {
    pub use axum::*;
}

pub mod axum_extra {
    pub use axum_extra::*;
}

/// A router wrapper that defers merging until `with_state()` is called.
///
/// This is necessary because in Axum, routers can only be merged when they have
/// the same state type. By deferring the merge, we ensure that:
/// 1. The base router's `.with_state()` is called first, converting it to `Router<()>`
/// 2. Then the child routers (also `Router<()>`) are merged
///
/// This wrapper is returned by `vespera!()` when the `merge` parameter is used.
pub struct VesperaRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    base: axum::Router<S>,
    /// Routers to merge after `with_state()` is called
    merge_fns: Vec<fn() -> axum::Router<()>>,
}

impl<S> VesperaRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Create a new VesperaRouter with a base router and routers to merge
    pub fn new(base: axum::Router<S>, merge_fns: Vec<fn() -> axum::Router<()>>) -> Self {
        Self { base, merge_fns }
    }

    /// Provide the state for the router and merge all child routers.
    ///
    /// This is equivalent to calling `Router::with_state()` and then merging
    /// all the child routers.
    ///
    /// After calling `with_state()`, the router's state type becomes `()` because
    /// the state has been provided. Child routers (also `Router<()>`) can then be merged.
    pub fn with_state(self, state: S) -> axum::Router<()> {
        // First, apply the state to convert Router<S> to Router<()>
        let mut router: axum::Router<()> = self.base.with_state(state);

        // Then merge all child routers (they are Router<()> which can be merged
        // into Router<()> without issues)
        for merge_fn in self.merge_fns {
            router = router.merge(merge_fn());
        }

        router
    }

    /// Add a layer to the router.
    pub fn layer<L>(self, layer: L) -> Self
    where
        L: tower_layer::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower_service::Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as tower_service::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse + 'static,
        <L::Service as tower_service::Service<axum::extract::Request>>::Error:
            Into<std::convert::Infallible> + 'static,
        <L::Service as tower_service::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        Self {
            base: self.base.layer(layer),
            merge_fns: self.merge_fns,
        }
    }
}

// Re-export tower_layer and tower_service for the layer method
pub use tower_layer;
pub use tower_service;
