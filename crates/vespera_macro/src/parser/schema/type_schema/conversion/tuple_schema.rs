//! The JSON array schema of a Rust tuple.

use std::{borrow::Borrow, collections::HashMap, collections::HashSet, hash::Hash};

use syn::Type;
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::parse_type_to_schema_ref;

/// Fixed-length JSON array for a Rust tuple / tuple struct / tuple variant:
/// `prefixItems` (one schema per position) with `minItems == maxItems == len`.
pub(super) fn tuple_array_schema<'t>(
    elems: impl ExactSizeIterator<Item = &'t Type>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let len = elems.len();
    let prefix_items: Vec<SchemaRef> = elems
        .map(|ty| parse_type_to_schema_ref(ty, known_schemas, struct_definitions))
        .collect();
    Schema {
        prefix_items: Some(prefix_items),
        min_items: Some(len),
        max_items: Some(len),
        items: None,
        ..Schema::new(SchemaType::Array)
    }
}
