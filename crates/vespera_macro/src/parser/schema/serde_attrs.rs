//! Serde attribute extraction utilities for OpenAPI schema generation.

mod common;
mod enum_repr;
mod extract;
mod fallback;
mod rename_case;

pub use common::{
    capitalize_first, extract_doc_comment, extract_schema_name_from_entity,
    extract_schema_ref_override, extract_transparent, strip_raw_prefix_owned,
};
pub use enum_repr::{SerdeEnumRepr, extract_enum_repr, extract_untagged};
pub use extract::{
    SerdeSide, extract_default, extract_deny_unknown_fields, extract_field_rename,
    extract_field_rename_for, extract_flatten, extract_rename_all, extract_rename_all_fields,
    extract_rename_all_for, extract_serde_skip_flags, extract_skip,
};
pub use rename_case::{rename_field, rename_variant};
