//! Schema generation module for `OpenAPI`.
//!
//! This module provides functionality for converting Rust types (structs, enums)
//! into OpenAPI-compatible JSON Schema definitions.
//!
//! # Overview
//!
//! The schema module is responsible for the critical task of converting arbitrary Rust types
//! into JSON Schema representations that can be included in `OpenAPI` specs. It handles:
//! - Primitive types (String, i32, bool, etc.)
//! - Complex types (Vec, Option, `HashMap`)
//! - User-defined structs and enums
//! - Serde attribute processing (rename, `rename_all`, default, skip)
//! - Generic type parameters
//! - Circular reference detection
//!
//! # Module Structure
//!
//! - `serde_attrs` - Extract serde attributes (`rename_all`, skip, default, etc.)
//! - `generics` - Generic type parameter substitution
//! - `struct_schema` - Struct to JSON Schema conversion
//! - `enum_schema` - Enum to JSON Schema conversion
//! - `type_schema` - Type to `SchemaRef` conversion (main entry point)
//!
//! # Key Functions
//!
//! - `parse_type_to_schema_ref` - Convert any Rust type to `SchemaRef`
//! - [`parse_struct_to_schema`] - Convert struct to JSON Schema object
//! - [`parse_enum_to_schema`] - Convert enum to JSON Schema (oneOf or enum array)
//! - [`extract_rename_all`] - Extract serde `rename_all` attribute

mod enum_schema;
mod garde_rules;
mod generics;
pub mod request_view;
pub mod schema_attrs;
mod serde_attrs;
mod struct_schema;
mod type_schema;
pub mod wire_override;

// Re-export public API
pub use enum_schema::{
    internally_tagged_non_object_message, parse_enum_to_schema, validate_enum_for_schema,
};
pub use generics::substitute_type;
pub use serde_attrs::{
    SerdeSide, extract_default, extract_doc_comment, extract_field_codec, extract_field_rename,
    extract_field_rename_for, extract_flatten, extract_rename_all, extract_rename_all_for,
    extract_serde_skip_flags, extract_skip, rename_field, strip_raw_prefix_owned,
};
pub use struct_schema::{apply_field_rules, parse_struct_to_schema};
// Re-export for internal use within the parser module. `parse_type_to_schema_ref`
// is reached directly via the `type_schema` submodule path where needed.
pub use type_schema::generic_instances;
pub use type_schema::{is_builtin_openapi_type, is_primitive_type, parse_type_to_schema_ref};
