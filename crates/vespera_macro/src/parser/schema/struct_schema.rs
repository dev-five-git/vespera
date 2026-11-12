//! Struct to JSON Schema conversion for `OpenAPI` generation.
//!
//! This module handles the conversion of Rust structs (as parsed by syn)
//! into OpenAPI-compatible JSON Schema definitions.

use std::{
    borrow::Borrow,
    collections::{BTreeMap, HashMap, HashSet},
    hash::Hash,
};

use syn::Fields;
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

use super::{
    schema_attrs::{SchemaConstraints, extract_schema_constraints},
    serde_attrs::{
        extract_doc_comment, extract_field_rename, extract_flatten, extract_rename_all,
        extract_schema_ref_override, extract_serde_skip_flags, extract_transparent, rename_field,
        strip_raw_prefix_owned,
    },
    type_schema::{parse_type_to_schema_ref, tuple_array_schema},
};
use crate::schema_macro::type_utils::is_option_type;

/// Parses a Rust struct into an `OpenAPI` Schema.
///
/// This function extracts:
/// - Field names and types as properties
/// - Required fields (non-`Option` types; `#[serde(default)]` does NOT relax
///   `required`, since this schema is shared by request and response bodies)
/// - Doc comments as descriptions
/// - Serde attributes (rename, `rename_all`, skip, default)
///
/// Tuple structs follow serde's wire shape: a newtype `struct Id(u64)` is its
/// inner schema, a multi-field `struct Pair(A, B)` is a fixed-length array.
///
/// # Arguments
/// * `struct_item` - The parsed struct from syn
/// * `known_schemas` - Map of known schema names for reference resolution
/// * `struct_definitions` - Map of struct names to their source code (for generics)
pub fn parse_struct_to_schema(
    struct_item: &syn::ItemStruct,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    // Extract struct-level doc comment for schema description
    let struct_description = extract_doc_comment(&struct_item.attrs);

    if let Some((schema_name, nullable)) = extract_schema_ref_override(&struct_item.attrs) {
        return Schema {
            ref_path: Some(format!("#/components/schemas/{schema_name}")),
            nullable: nullable.then_some(true),
            description: struct_description,
            ..Default::default()
        };
    }

    // Transparent wrappers (serde requires exactly one non-skipped field) and
    // newtype tuple structs (serde decides "newtype" by the DECLARED field
    // count) serialize as their inner value, so they use the inner schema.
    let transparent = extract_transparent(&struct_item.attrs);
    let inner_field_ty = match &struct_item.fields {
        Fields::Named(fields_named) if transparent => {
            single_emitted_type(fields_named.named.iter())
        }
        Fields::Unnamed(fields_unnamed) if transparent => {
            single_emitted_type(fields_unnamed.unnamed.iter())
        }
        Fields::Unnamed(fields_unnamed) if fields_unnamed.unnamed.len() == 1 => {
            fields_unnamed.unnamed.first().map(|field| &field.ty)
        }
        _ => None,
    };
    if let Some(field_ty) = inner_field_ty {
        let schema_ref = parse_type_to_schema_ref(field_ty, known_schemas, struct_definitions);
        return with_description(schema_ref, struct_description);
    }

    match &struct_item.fields {
        Fields::Named(fields_named) => {
            let rename_all = extract_rename_all(&struct_item.attrs);
            collect_named_fields(
                fields_named,
                rename_all.as_deref(),
                known_schemas,
                struct_definitions,
            )
            .into_object_schema(struct_description)
        }
        // `struct Pair(A, B)` → `[A, B]` (fully skipped positions are not
        // serialized, so they are not array items either).
        Fields::Unnamed(fields_unnamed) => Schema {
            description: struct_description,
            ..tuple_array_schema(
                emitted_unnamed_types(fields_unnamed).into_iter(),
                known_schemas,
                struct_definitions,
            )
        },
        Fields::Unit => Schema {
            description: struct_description,
            ..Schema::object_empty()
        },
    }
}

/// Types of the tuple fields that appear on the wire: fully skipped fields
/// (`#[serde(skip)]`, or `skip_serializing` + `skip_deserializing`) are
/// dropped, matching serde's tuple-struct / tuple-variant length.
pub(super) fn emitted_unnamed_types(fields: &syn::FieldsUnnamed) -> Vec<&syn::Type> {
    fields
        .unnamed
        .iter()
        .filter(|field| !extract_serde_skip_flags(&field.attrs).fully_skipped())
        .map(|field| &field.ty)
        .collect()
}

/// The type of the ONLY non-skipped field, if exactly one remains.
fn single_emitted_type<'a>(fields: impl Iterator<Item = &'a syn::Field>) -> Option<&'a syn::Type> {
    let mut emitted =
        fields.filter(|field| !extract_serde_skip_flags(&field.attrs).fully_skipped());
    let first = emitted.next()?;
    emitted.next().is_none().then_some(&first.ty)
}

/// Turn a `SchemaRef` into a `Schema`, attaching `description` (a `$ref` is
/// wrapped in `allOf` so the description can sit beside it). An inline
/// schema's own description wins.
pub(super) fn with_description(schema_ref: SchemaRef, description: Option<String>) -> Schema {
    match schema_ref {
        SchemaRef::Inline(mut schema) => {
            if schema.description.is_none() {
                schema.description = description;
            }
            *schema
        }
        SchemaRef::Ref(reference) => Schema {
            description,
            all_of: Some(vec![SchemaRef::Ref(reference)]),
            ..Default::default()
        },
    }
}

/// Properties collected from a set of named fields — shared by structs and
/// enum struct-variants so both honour the same field attributes.
#[derive(Default)]
pub(super) struct NamedFields {
    pub properties: BTreeMap<String, SchemaRef>,
    pub required: Vec<String>,
    /// `#[serde(flatten)]` field schemas, composed via `allOf`.
    pub flattened: Vec<SchemaRef>,
}

impl NamedFields {
    /// Object schema for these fields; flattened fields turn the result into
    /// `allOf: [<own properties>, ...flattened]`.
    pub(super) fn into_object_schema(self, description: Option<String>) -> Schema {
        let inline_schema = Schema {
            schema_type: Some(SchemaType::Object),
            properties: (!self.properties.is_empty()).then_some(self.properties),
            required: (!self.required.is_empty()).then_some(self.required),
            ..Schema::object_empty()
        };
        if self.flattened.is_empty() {
            return Schema {
                description,
                ..inline_schema
            };
        }
        let mut all_of = vec![SchemaRef::Inline(Box::new(inline_schema))];
        all_of.extend(self.flattened);
        Schema {
            description,
            all_of: Some(all_of),
            ..Default::default()
        }
    }
}

/// Walk named fields applying every field-level attribute vespera understands:
/// `#[serde(skip)]` (and `skip_serializing` + `skip_deserializing`),
/// `#[serde(flatten)]`, `#[serde(rename)]` / `rename_all`, doc comments,
/// `#[schema(...)]` constraints (incl. `any`), and `required` inference
/// (`Option<T>`, `skip_serializing`, and `skip_serializing_if` fields are not
/// required).
pub(super) fn collect_named_fields(
    fields_named: &syn::FieldsNamed,
    rename_all: Option<&str>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> NamedFields {
    let mut out = NamedFields {
        required: Vec::with_capacity(fields_named.named.len()),
        ..NamedFields::default()
    };
    for field in &fields_named.named {
        let skip_flags = extract_serde_skip_flags(&field.attrs);
        if skip_flags.fully_skipped() {
            continue;
        }

        if extract_flatten(&field.attrs) {
            out.flattened.push(parse_type_to_schema_ref(
                &field.ty,
                known_schemas,
                struct_definitions,
            ));
            continue;
        }

        let rust_field_name = field.ident.as_ref().map_or_else(
            || "unknown".to_string(),
            |i| strip_raw_prefix_owned(i.to_string()),
        );
        // Field-level rename takes precedence over rename_all.
        let field_name = extract_field_rename(&field.attrs)
            .unwrap_or_else(|| rename_field(&rust_field_name, rename_all));

        // A field marked `#[schema(any)]` SHORT-CIRCUITS type conversion: the
        // emitted schema is the generic `{type: "object"}` regardless of the
        // Rust type (matching the derive-Schema leaf-type assertion that also
        // exempts `any` fields).
        let constraints = extract_schema_constraints(&field.attrs);
        let mut schema_ref = if constraints.is_any() {
            SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
        } else {
            parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions)
        };

        // Doc comment → description (a `$ref` is wrapped in `allOf`).
        if let Some(doc) = extract_doc_comment(&field.attrs) {
            schema_ref = match schema_ref {
                SchemaRef::Inline(mut schema) => {
                    schema.description = Some(doc);
                    SchemaRef::Inline(schema)
                }
                SchemaRef::Ref(reference) => SchemaRef::Inline(Box::new(Schema {
                    description: Some(doc),
                    all_of: Some(vec![SchemaRef::Ref(reference)]),
                    ..Default::default()
                })),
            };
        }

        // Field-level `#[schema(min_length=..., pattern=..., ...)]` constraints.
        if !constraints.is_empty() {
            apply_constraints_to_schema_ref(&mut schema_ref, &constraints);
        }

        // Required is determined by nullability (Option<T>) and by whether the
        // field can be omitted from serialized output (`skip_serializing`,
        // `skip_serializing_if`). `#[serde(default)]` does NOT relax required:
        // this schema is shared by request AND response bodies, and a
        // defaulted field is always present on output (documented in README;
        // the query extractor differs because query params are input-only).
        if !is_option_type(&field.ty) && !skip_flags.maybe_omitted_on_output() {
            out.required.push(field_name.clone());
        }

        out.properties.insert(field_name, schema_ref);
    }
    out
}

/// Merge field-level `#[schema(...)]` constraints into the field's
/// `SchemaRef`.  For `Inline` variants the constraints are written
/// directly onto the inner `Schema`; for `Ref` variants we promote to an
/// `allOf` wrapper so the constraints can sit alongside `$ref`.
fn apply_constraints_to_schema_ref(schema_ref: &mut SchemaRef, c: &SchemaConstraints) {
    match schema_ref {
        SchemaRef::Inline(schema) => apply_constraints(schema, c),
        SchemaRef::Ref(_) => {
            // mem::replace lets us move the Ref out without leaving an
            // invalid value behind; the placeholder is overwritten
            // before the function returns.
            let taken = std::mem::replace(
                schema_ref,
                SchemaRef::Inline(Box::new(Schema::object_empty())),
            );
            if let SchemaRef::Ref(reference) = taken {
                let mut wrapper = Schema {
                    all_of: Some(vec![SchemaRef::Ref(reference)]),
                    ..Default::default()
                };
                apply_constraints(&mut wrapper, c);
                *schema_ref = SchemaRef::Inline(Box::new(wrapper));
            }
        }
    }
}

/// Apply each set constraint to the corresponding `Schema` field.
fn apply_constraints(schema: &mut Schema, c: &SchemaConstraints) {
    if let Some(v) = c.min_length {
        schema.min_length = Some(v);
    }
    if let Some(v) = c.max_length {
        schema.max_length = Some(v);
    }
    if let Some(ref v) = c.pattern {
        schema.pattern = Some(v.clone());
    }
    if let Some(v) = c.minimum {
        schema.minimum = Some(v);
    }
    if let Some(v) = c.maximum {
        schema.maximum = Some(v);
    }
    if c.exclusive_minimum == Some(true) {
        schema.exclusive_minimum = c.minimum;
    }
    if c.exclusive_maximum == Some(true) {
        schema.exclusive_maximum = c.maximum;
    }
    if let Some(v) = c.multiple_of {
        schema.multiple_of = Some(v);
    }
    if let Some(v) = c.min_items {
        schema.min_items = Some(v);
    }
    if let Some(v) = c.max_items {
        schema.max_items = Some(v);
    }
    if let Some(v) = c.unique_items {
        schema.unique_items = Some(v);
    }
    if let Some(ref v) = c.format {
        schema.format = Some(v.clone());
    }
    if let Some(ref v) = c.example {
        schema.example = Some(v.clone());
    }
    if let Some(v) = c.read_only {
        schema.read_only = Some(v);
    }
    if let Some(v) = c.write_only {
        schema.write_only = Some(v);
    }
}

#[cfg(test)]
mod tests;
