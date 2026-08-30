//! Router code generation and macro input parsing.
//!
//! Public API is re-exported from child modules to preserve
//! `crate::router_codegen::...` call paths.

mod docs;
mod export;
mod generator;
mod input;

pub use export::{
    ExportAppInput, apply_export_prefix, namespace_export_schemas, schema_namespace_from_prefix,
};
pub use generator::generate_router_code;
pub use input::{AutoRouterInput, ProcessedVesperaInput, process_vespera_input};
