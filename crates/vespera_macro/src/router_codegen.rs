//! Router code generation and macro input parsing.
//!
//! Public API is re-exported from child modules to preserve
//! `crate::router_codegen::...` call paths.

mod docs;
mod export;
mod generator;
mod info;
mod input;
mod route_dir;
mod security_dsl;

pub use export::{
    ExportAppInput, apply_export_prefix, namespace_export_schemas, schema_namespace_from_prefix,
};
pub use generator::generate_router_code;
#[cfg(test)]
pub use info::InfoArgs;
pub use input::{AutoRouterInput, ProcessedVesperaInput, process_vespera_input};
pub use route_dir::{route_dir_module_path, validate_route_dir};
