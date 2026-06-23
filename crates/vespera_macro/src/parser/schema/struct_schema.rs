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
        extract_schema_ref_override, extract_skip, extract_transparent, rename_field,
        strip_raw_prefix_owned,
    },
    type_schema::parse_type_to_schema_ref,
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
/// # Arguments
/// * `struct_item` - The parsed struct from syn
/// * `known_schemas` - Map of known schema names for reference resolution
/// * `struct_definitions` - Map of struct names to their source code (for generics)
#[allow(clippy::too_many_lines)]
pub fn parse_struct_to_schema(
    struct_item: &syn::ItemStruct,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut properties = BTreeMap::new();
    let mut required = Vec::with_capacity(8);
    let mut flattened_refs: Vec<SchemaRef> = Vec::new();

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

    // Transparent single-field wrappers should use the inner field schema directly.
    if extract_transparent(&struct_item.attrs) {
        let inner_field_ty = match &struct_item.fields {
            Fields::Named(fields_named) if fields_named.named.len() == 1 => {
                fields_named.named.first().map(|field| &field.ty)
            }
            Fields::Unnamed(fields_unnamed) if fields_unnamed.unnamed.len() == 1 => {
                fields_unnamed.unnamed.first().map(|field| &field.ty)
            }
            _ => None,
        };

        if let Some(field_ty) = inner_field_ty {
            let schema_ref = parse_type_to_schema_ref(field_ty, known_schemas, struct_definitions);
            return match schema_ref {
                SchemaRef::Inline(mut schema) => {
                    if schema.description.is_none() {
                        schema.description = struct_description;
                    }
                    *schema
                }
                SchemaRef::Ref(reference) => Schema {
                    description: struct_description,
                    all_of: Some(vec![SchemaRef::Ref(reference)]),
                    ..Default::default()
                },
            };
        }
    }

    // Extract rename_all attribute from struct
    let rename_all = extract_rename_all(&struct_item.attrs);

    match &struct_item.fields {
        Fields::Named(fields_named) => {
            for field in &fields_named.named {
                // Check if field should be skipped
                if extract_skip(&field.attrs) {
                    continue;
                }

                // Check if field should be flattened
                if extract_flatten(&field.attrs) {
                    // Get the schema ref for the flattened field type
                    let field_type = &field.ty;
                    let schema_ref =
                        parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

                    // Add to flattened refs for allOf composition
                    flattened_refs.push(schema_ref);
                    continue;
                }

                let rust_field_name = field.ident.as_ref().map_or_else(
                    || "unknown".to_string(),
                    |i| strip_raw_prefix_owned(i.to_string()),
                );

                // Check for field-level rename attribute first (takes precedence)
                let field_name = extract_field_rename(&field.attrs)
                    .unwrap_or_else(|| rename_field(&rust_field_name, rename_all.as_deref()));

                let field_type = &field.ty;

                let mut schema_ref =
                    parse_type_to_schema_ref(field_type, known_schemas, struct_definitions);

                // Extract doc comment from field and set as description
                if let Some(doc) = extract_doc_comment(&field.attrs) {
                    match &mut schema_ref {
                        SchemaRef::Inline(schema) => {
                            schema.description = Some(doc);
                        }
                        SchemaRef::Ref(_) => {
                            // For $ref schemas, we need to wrap in an allOf to add description
                            // OpenAPI 3.1 allows siblings to $ref, so we can add description directly
                            // by converting to inline schema with description + allOf[$ref]
                            let ref_schema = std::mem::replace(
                                &mut schema_ref,
                                SchemaRef::Inline(Box::new(Schema::object())),
                            );
                            if let SchemaRef::Ref(reference) = ref_schema {
                                schema_ref = SchemaRef::Inline(Box::new(Schema {
                                    description: Some(doc),
                                    all_of: Some(vec![SchemaRef::Ref(reference)]),
                                    ..Default::default()
                                }));
                            }
                        }
                    }
                }

                // Extract field-level `#[schema(min_length=..., pattern=...,
                // minimum=..., format=..., example=..., read_only, ...)]`
                // constraints and merge them into the field schema.  When
                // the field references a component schema via `$ref`, we
                // promote it to an `allOf` wrapper (mirroring the
                // description-on-ref pattern above) so the constraints can
                // sit alongside the reference.
                let constraints = extract_schema_constraints(&field.attrs);
                if !constraints.is_empty() {
                    apply_constraints_to_schema_ref(&mut schema_ref, &constraints);
                }

                // Required is determined solely by nullability (Option<T>).
                // Fields with #[serde(default)] still have defaults applied in
                // openapi_generator, but that does NOT affect required status:
                // this schema is shared by request AND response bodies, and a
                // defaulted field is always present on output, so it stays
                // required (deliberate, documented in README; the query
                // extractor differs because query params are input-only).
                let is_optional = is_option_type(field_type);

                if !is_optional {
                    required.push(field_name.clone());
                }

                properties.insert(field_name, schema_ref);
            }
        }
        Fields::Unnamed(_) | Fields::Unit => {
            // Tuple structs and unit structs have no named fields
        }
    }

    // If there are flattened fields, use allOf composition
    if flattened_refs.is_empty() {
        // No flattened fields - return normal schema
        Schema {
            schema_type: Some(SchemaType::Object),
            description: struct_description,
            properties: if properties.is_empty() {
                None
            } else {
                Some(properties)
            },
            required: if required.is_empty() {
                None
            } else {
                Some(required)
            },
            ..Schema::object()
        }
    } else {
        // Create the inline schema for non-flattened properties
        let inline_schema = Schema {
            schema_type: Some(SchemaType::Object),
            properties: if properties.is_empty() {
                None
            } else {
                Some(properties)
            },
            required: if required.is_empty() {
                None
            } else {
                Some(required)
            },
            ..Schema::object()
        };

        // Build allOf: [inline_schema, ...flattened_refs]
        let mut all_of = vec![SchemaRef::Inline(Box::new(inline_schema))];
        all_of.extend(flattened_refs);

        Schema {
            description: struct_description,
            all_of: Some(all_of),
            ..Default::default()
        }
    }
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
            let taken =
                std::mem::replace(schema_ref, SchemaRef::Inline(Box::new(Schema::object())));
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
