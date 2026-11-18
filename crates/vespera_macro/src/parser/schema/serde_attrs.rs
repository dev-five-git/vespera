//! Serde attribute extraction utilities for OpenAPI schema generation.

mod codec;
mod common;
mod enum_repr;
mod extract;
mod fallback;
mod rename_case;

pub use codec::{
    extract_codec_module, extract_container_conversion, extract_field_codec, skips_none_on_output,
};
pub use common::{
    EntityTarget, capitalize_first, entity_target, extract_doc_comment, extract_schema_default,
    extract_schema_ref_override, extract_transparent, strip_raw_prefix_owned,
};
pub use enum_repr::{SerdeEnumRepr, extract_enum_repr, extract_tag, extract_untagged};
pub use extract::{
    SerdeSide, derives_serialize, extract_aliases, extract_default, extract_deny_unknown_fields,
    extract_field_rename, extract_field_rename_for, extract_field_rename_owned, extract_flatten,
    extract_rename_all, extract_rename_all_fields_for, extract_rename_all_for,
    extract_serde_skip_flags, extract_skip,
};
pub use rename_case::{rename_field, rename_variant};
