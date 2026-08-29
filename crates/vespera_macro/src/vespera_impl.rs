//! Core implementation of vespera! and `export_app`! macros.
//!
//! The public orchestrator entry points are re-exported from the
//! `orchestrator` child module; every other helper stays crate-internal and is
//! imported directly from its owning child module.

mod cache;
mod openapi_io;
mod orchestrator;
mod path_utils;
mod route_merge;
mod schema_merge;

pub use orchestrator::{process_export_app, process_vespera_macro};
