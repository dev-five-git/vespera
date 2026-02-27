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
//! - [`parse_type_to_schema_ref`] - Convert any Rust type to `SchemaRef`
//! - [`parse_struct_to_schema`] - Convert struct to JSON Schema object
//! - [`parse_enum_to_schema`] - Convert enum to JSON Schema (oneOf or enum array)
//! - [`extract_rename_all`] - Extract serde `rename_all` attribute

mod enum_schema;
mod generics;
mod serde_attrs;
mod struct_schema;
mod type_schema;

// Re-export public API
pub use enum_schema::parse_enum_to_schema;
pub use serde_attrs::{
    extract_default, extract_field_rename, extract_rename_all, extract_skip,
    extract_skip_serializing_if, rename_field, strip_raw_prefix_owned,
};
pub use struct_schema::parse_struct_to_schema;
pub use type_schema::parse_type_to_schema_ref;
// Re-export for internal use within parser module
pub use type_schema::{is_primitive_type, parse_type_to_schema_ref_with_schemas};
