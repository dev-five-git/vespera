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

// Re-export macros from vespera_macro
pub use vespera_macro::{Schema, route, vespera};

// Re-export axum for convenience
pub mod axum {
    pub use axum::*;
}

pub mod axum_extra {
    pub use axum_extra::*;
}
