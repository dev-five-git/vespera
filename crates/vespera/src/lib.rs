//! Vespera - `OpenAPI` generation for Rust web frameworks
//!
//! This crate provides macros and utilities for generating `OpenAPI` documentation
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
pub use vespera_macro::{Multipart, Schema, cron, export_app, route, schema, schema_type, vespera};

/// Marker trait implemented by every `#[derive(Schema)]` type.
///
/// The derive macro auto-implements this trait, which is intentionally
/// empty — it carries no methods.  Its purpose is to anchor the
/// compile-time leaf-type assertions emitted by `#[derive(Schema)]`:
/// for every field whose type is not a builtin OpenAPI primitive,
/// not `serde_json::Value`, and not marked `#[schema(any)]`, the
/// derive emits a `T: ::vespera::Schema` bound assertion against the
/// field's leaf type.  An unbound leaf — typically a custom struct
/// that forgot its own `#[derive(Schema)]` — becomes a compile error
/// at the field site instead of silently emitting `{type:object}`
/// into the OpenAPI document.
///
/// Users normally never name this trait directly — `#[derive(Schema)]`
/// is the entire user surface.  If you intentionally want a field to
/// stay as opaque `{type:object}` (arbitrary JSON), mark it with
/// `#[schema(any)]` to skip the assertion AND lock the schema to
/// `object`.  `serde_json::Value` fields are allowlisted automatically.
///
/// The trait and the `vespera::Schema` derive macro share the same
/// name but live in different namespaces (trait vs. derive-macro), so
/// the existing `#[derive(Schema)]` syntax continues to work
/// unchanged.
pub trait Schema {}

// Re-export serde_json for merge feature (runtime spec merging)
pub use serde_json;

// Re-export chrono for schema_type! datetime conversion
// This allows generated types to use chrono::DateTime without users adding chrono dependency
pub use chrono;

// Native multipart form data extraction (replaces axum_typed_multipart)
pub mod multipart;

// Re-export tempfile for schema_type! multipart mode (NamedTempFile)
pub use tempfile;

// Re-export tokio-cron-scheduler for cron job support
#[cfg(feature = "cron")]
pub use tokio_cron_scheduler;

// Re-export tokio for cron scheduler spawning
#[cfg(feature = "cron")]
pub use tokio;

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
    /// Layers deferred until **after** child routers are merged.
    ///
    /// Axum's `Router::layer` only wraps the routes present at call
    /// time, so applying a layer eagerly to `base` would leave
    /// `merge`d child routes un-layered (CORS / auth / trace silently
    /// skipped on merged routes).  Storing the layer as a closure and
    /// replaying it in `with_state()` after the merge guarantees it
    /// covers every route.  Each closure captures only the layer value
    /// (`L: Send + Sync`), so the boxed trait object stays `Send + Sync`
    /// and `VesperaRouter` keeps its previous auto-trait bounds.
    layers: Vec<Box<dyn FnOnce(axum::Router) -> axum::Router + Send + Sync>>,
}

impl<S> VesperaRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Create a `VesperaRouter` from a base router and the child-app router
    /// factories to merge into it.
    ///
    /// This is invoked by the `vespera!` macro when the `merge = [...]`
    /// parameter is used; it is rarely constructed directly. Both the merge of
    /// the child routers and any [`layer`](Self::layer) added afterwards are
    /// **deferred** until [`with_state`](Self::with_state): Axum can only merge
    /// routers that share a state type, so the base router's state must be
    /// applied first. When a `vespera!` app has no `merge` entries the macro
    /// returns a plain `axum::Router` instead of this wrapper.
    #[must_use]
    pub fn new(base: axum::Router<S>, merge_fns: Vec<fn() -> axum::Router<()>>) -> Self {
        Self {
            base,
            merge_fns,
            layers: Vec::new(),
        }
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

        // Finally replay the deferred layers AFTER the merge so they wrap
        // both the base routes and every merged child route.  Applied in
        // insertion order, preserving Axum's "last layer is outermost"
        // semantics identical to chained `Router::layer` calls.
        for apply in self.layers {
            router = apply(router);
        }

        router
    }

    /// Add a layer to the router.
    ///
    /// The layer is **deferred** and applied in [`with_state`](Self::with_state)
    /// after child routers are merged, so it covers merged routes as well as
    /// the base router.
    #[must_use]
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: tower_layer::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower_service::Service<axum::extract::Request> + Clone + Send + Sync + 'static,
        <L::Service as tower_service::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse + 'static,
        <L::Service as tower_service::Service<axum::extract::Request>>::Error:
            Into<std::convert::Infallible> + 'static,
        <L::Service as tower_service::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.layers
            .push(Box::new(move |router: axum::Router| router.layer(layer)));
        self
    }
}

// Re-export tower_layer and tower_service for the layer method
pub use tower_layer;
pub use tower_service;

/// Runtime validation — private re-export of `garde` used by the
/// `#[derive(Schema)]` codegen.  Users never reference this module
/// directly; it exists so the macro-emitted impl bodies stay inside the
/// `vespera` namespace and so we retain the freedom to swap the
/// validator backend later without touching user code.
#[cfg(feature = "validation")]
#[doc(hidden)]
pub mod __validation;

/// [`Validated<T>`] extractor — wraps any axum extractor and runs
/// `garde::Validate` on the inner payload before the handler is called.
/// Failure produces `422 Unprocessable Entity` with a JSON error envelope.
#[cfg(feature = "validation")]
mod validated;
#[cfg(feature = "validation")]
pub use validated::{
    ValidatePayload, ValidatePayloadWith, Validated, ValidatedWith, ValidationContext,
};

/// In-process dispatch — drive an axum Router without a TCP socket.
#[cfg(feature = "inprocess")]
pub use vespera_inprocess as inprocess;

/// One-liner `Router::serve(addr)` extension — see [`serve::Serve`].
pub mod serve;
pub use serve::Serve;

/// JNI bridge — call Rust axum apps from Java.
#[cfg(feature = "jni")]
pub use vespera_jni as jni;

/// Generate the `JNI_OnLoad` export that registers your app
/// (single-app, default).
///
/// ```ignore
/// vespera::jni_app!(create_app);
/// ```
#[cfg(feature = "jni")]
#[macro_export]
macro_rules! jni_app {
    ($factory:expr) => {
        $crate::jni::jni_app!($factory);
    };
}

/// Generate the `JNI_OnLoad` export that registers **multiple named
/// apps** for multi-app routing.  See [`vespera_jni::jni_apps!`] for
/// details.
///
/// ```ignore
/// vespera::jni_apps! {
///     "admin"  => admin_app,
///     "public" => public_app,
/// }
/// ```
#[cfg(feature = "jni")]
#[macro_export]
macro_rules! jni_apps {
    ( $( $name:literal => $factory:expr ),+ $(,)? ) => {
        $crate::jni::jni_apps! {
            $( $name => $factory ),+
        }
    };
}
