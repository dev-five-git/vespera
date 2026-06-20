//! Core implementation of vespera! and `export_app`! macros.
//!
//! Public orchestrators and helper functions are re-exported from child
//! modules to preserve `crate::vespera_impl::...` call paths.

mod cache;
mod openapi_io;
mod orchestrator;
mod path_utils;
mod route_merge;

#[allow(unused_imports)]
pub use openapi_io::{
    OpenApiWriteResult, ensure_openapi_files_from_cache, generate_and_write_openapi,
};
pub use orchestrator::{process_export_app, process_vespera_macro};
#[allow(unused_imports)]
pub use path_utils::{find_folder_path, find_target_dir};
