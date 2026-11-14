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
use vespera_core::schema::{AdditionalProperties, Schema, SchemaRef, SchemaType};

use super::{
    garde_rules::{apply_garde_rules, extract_garde_rules},
    schema_attrs::{SchemaConstraints, extract_schema_constraints},
    serde_attrs::{
        SerdeSide, extract_default, extract_deny_unknown_fields, extract_doc_comment,
        extract_field_codec, extract_field_rename_for, extract_flatten, extract_rename_all,
        extract_rename_all_for, extract_schema_ref_override, extract_serde_skip_flags,
        extract_transparent, rename_field, strip_raw_prefix_owned,
    },
    type_schema::{parse_type_to_schema_ref, tuple_array_schema},
    wire_override::{current_container_override, field_schema_ref},
};
use crate::schema_macro::type_utils::{is_option_type, option_inner};

/// Parses a Rust struct into an `OpenAPI` Schema.
///
/// This function extracts:
/// - Field names and types as properties
/// - Required fields (non-`Option` types). In the response view a
///   `#[serde(default)]` field stays present on output; inside
///   `request_view::with_request_view` (the `{Name}Input` view) it is optional
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
    let params: Vec<syn::Type> = struct_item
        .generics
        .type_params()
        .map(|param| {
            let ident = &param.ident;
            syn::parse_quote!(#ident)
        })
        .collect();
    if params.is_empty() {
        return parse_struct_body(struct_item, known_schemas, struct_definitions);
    }
    // The generic definition's own component: `Tree<T>` inside `Tree<T>`
    // refers back to `Tree` instead of materialising `Tree_T`.
    let component = crate::schema_impl::extract_schema_name_attr(&struct_item.attrs)
        .unwrap_or_else(|| struct_item.ident.to_string());
    let instance = super::generic_instances::instance_name(&component, &params);
    super::generic_instances::with_self_instance(instance, component, || {
        parse_struct_body(struct_item, known_schemas, struct_definitions)
    })
}

fn parse_struct_body(
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

    // `#[schema(value_type = T)]`, or serde `into` (response view) /
    // `from` / `try_from` (request view): the wire shape is `T`'s.
    if let Some(ty) = current_container_override(&struct_item.attrs) {
        let schema_ref = parse_type_to_schema_ref(&ty, known_schemas, struct_definitions);
        return with_description(schema_ref, struct_description);
    }

    // Transparent wrappers (serde requires exactly one non-skipped field) and
    // newtype tuple structs (serde decides "newtype" by the DECLARED field
    // count) serialize as their inner value, so they use the inner schema.
    let transparent = extract_transparent(&struct_item.attrs);
    let inner_field = match &struct_item.fields {
        Fields::Named(fields_named) if transparent => {
            single_emitted_field(fields_named.named.iter())
        }
        Fields::Unnamed(fields_unnamed) if transparent => {
            single_emitted_field(fields_unnamed.unnamed.iter())
        }
        Fields::Unnamed(fields_unnamed) if fields_unnamed.unnamed.len() == 1 => {
            fields_unnamed.unnamed.first()
        }
        _ => None,
    };
    if let Some(field) = inner_field {
        let schema_ref = field_schema_ref(
            field,
            &extract_schema_constraints(&field.attrs),
            super::request_view::current_side(),
            known_schemas,
            struct_definitions,
        );
        return with_description(schema_ref, struct_description);
    }

    match &struct_item.fields {
        Fields::Named(fields_named) => {
            let request = super::request_view::in_request_view();
            let rename_all = if request {
                extract_rename_all_for(&struct_item.attrs, SerdeSide::Deserialize)
            } else {
                extract_rename_all(&struct_item.attrs)
            };
            let container_default = request && extract_default(&struct_item.attrs).is_some();
            let fields = collect_named_fields(
                fields_named,
                rename_all.as_deref(),
                container_default,
                known_schemas,
                struct_definitions,
            );

            // `deny_unknown_fields` → closed object. serde does not support
            // it together with `flatten`, so a flattening struct stays open.
            let closed =
                fields.flattened.is_empty() && extract_deny_unknown_fields(&struct_item.attrs);
            let mut schema = fields.into_object_schema(struct_description);
            if closed {
                schema.additional_properties = Some(AdditionalProperties::Bool(false));
            }
            schema
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
        // `struct Marker;` → serde writes (and reads) a unit struct as `null`.
        Fields::Unit => Schema {
            description: struct_description,
            ..Schema::new(SchemaType::Null)
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

/// The ONLY non-skipped field, if exactly one remains.
fn single_emitted_field<'a>(
    fields: impl Iterator<Item = &'a syn::Field>,
) -> Option<&'a syn::Field> {
    let mut emitted =
        fields.filter(|field| !extract_serde_skip_flags(&field.attrs).fully_skipped());
    let first = emitted.next()?;
    emitted.next().is_none().then_some(first)
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
    container_default: bool,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> NamedFields {
    let mut out = NamedFields {
        required: Vec::with_capacity(fields_named.named.len()),
        ..NamedFields::default()
    };
    // Request view (see `request_view`): fields serde never reads are
    // dropped, keys use the deserialize name, and `#[serde(default)]`
    // (field or container) makes a field optional.
    let request = super::request_view::in_request_view();
    for field in &fields_named.named {
        let skip_flags = extract_serde_skip_flags(&field.attrs);
        if skip_flags.fully_skipped() || (request && skip_flags.skip_deserializing) {
            continue;
        }

        if extract_flatten(&field.attrs) {
            out.flattened.push(option_inner(&field.ty).map_or_else(
                || parse_type_to_schema_ref(&field.ty, known_schemas, struct_definitions),
                |inner| optional_flattened_schema(inner, known_schemas, struct_definitions),
            ));
            continue;
        }

        let rust_field_name = field.ident.as_ref().map_or_else(
            || "unknown".to_string(),
            |i| strip_raw_prefix_owned(i.to_string()),
        );
        // Field-level rename takes precedence over rename_all.
        let side = if request {
            SerdeSide::Deserialize
        } else {
            SerdeSide::Serialize
        };
        let field_name = extract_field_rename_for(&field.attrs, side)
            .unwrap_or_else(|| rename_field(&rust_field_name, rename_all));

        // `#[schema(any)]` / `#[schema(value_type = T)]` / serde `with`
        // replace the Rust type's schema (see `wire_override`).
        let constraints = extract_schema_constraints(&field.attrs);
        let mut schema_ref =
            field_schema_ref(field, &constraints, side, known_schemas, struct_definitions);

        // Doc comment → description; Rust `#[deprecated]` → `deprecated`;
        // `skip_deserializing` (output only) → `readOnly`, `skip_serializing`
        // (input only) → `writeOnly`. A `$ref` is wrapped in `allOf` so the
        // annotations can sit beside it; explicit `#[schema(read_only, ..)]`
        // constraints applied below still win.
        let doc = extract_doc_comment(&field.attrs);
        let deprecated = field
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("deprecated"));
        // (Both are meaningless in a request-only view.)
        let read_only = !request && skip_flags.skip_deserializing;
        let write_only = !request && skip_flags.skip_serializing;
        if doc.is_some() || deprecated || read_only || write_only {
            let mut schema = match schema_ref {
                SchemaRef::Inline(schema) => schema,
                SchemaRef::Ref(reference) => Box::new(Schema {
                    all_of: Some(vec![SchemaRef::Ref(reference)]),
                    ..Default::default()
                }),
            };
            if doc.is_some() {
                schema.description = doc;
            }
            schema.deprecated = deprecated.then_some(true).or(schema.deprecated);
            schema.read_only = read_only.then_some(true).or(schema.read_only);
            schema.write_only = write_only.then_some(true).or(schema.write_only);
            schema_ref = SchemaRef::Inline(schema);
        }

        apply_field_rules(&mut schema_ref, field, &constraints);

        // Required is determined by nullability (Option<T>) and, in the
        // response view, by whether the field can be omitted from serialized
        // output (`skip_serializing`, `skip_serializing_if`); in the request
        // view by `#[serde(default)]` (field or container), since serde fills
        // a missing defaulted field in.
        let optional = if request {
            container_default || extract_default(&field.attrs).is_some()
        } else {
            skip_flags.maybe_omitted_on_output()
        };
        // serde fills an absent `Option` with `None` only when it reads the
        // field itself: a custom `deserialize_with` makes it mandatory.
        let absent_is_none = is_option_type(&field.ty)
            && !(request && extract_field_codec(&field.attrs).deserialize);
        if !absent_is_none && !optional {
            out.required.push(field_name.clone());
        }

        out.properties.insert(field_name, schema_ref);
    }
    out
}

/// Schema of a `#[serde(flatten)] Option<Inner>` field.
///
/// serde deserializes the flattened `Inner` only when all of its fields are
/// present and yields `None` otherwise, so none of `Inner`'s fields may be
/// `required` in the composed object.
fn optional_flattened_schema(
    inner: &syn::Type,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    without_required(
        parse_type_to_schema_ref(inner, known_schemas, struct_definitions),
        known_schemas,
        struct_definitions,
    )
}

/// `schema_ref` with every `required` list dropped, recursively through
/// `allOf` members. A `$ref` cannot be relaxed in place, so a referenced
/// known struct is inlined; a `$ref` without a parseable struct definition
/// (e.g. an enum) is kept as-is.
fn without_required(
    schema_ref: SchemaRef,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    let mut schema = match schema_ref {
        SchemaRef::Inline(schema) => *schema,
        SchemaRef::Ref(reference) => {
            let Some(struct_item) = reference
                .ref_path
                .strip_prefix("#/components/schemas/")
                .and_then(|name| struct_definitions.get(name))
                .and_then(|def| syn::parse_str::<syn::ItemStruct>(def.as_ref()).ok())
            else {
                return SchemaRef::Ref(reference);
            };
            parse_struct_to_schema(&struct_item, known_schemas, struct_definitions)
        }
    };
    schema.required = None;
    schema.all_of = schema.all_of.take().map(|members| {
        members
            .into_iter()
            .map(|member| without_required(member, known_schemas, struct_definitions))
            .collect()
    });
    SchemaRef::Inline(Box::new(schema))
}

/// Document a field's validation rules on its schema: the `#[garde(...)]`
/// rules a `garde::Validate` derive enforces (see [`super::garde_rules`]),
/// then the field-level `#[schema(min_length=..., pattern=..., ...)]`
/// constraints `c` (explicit — they win). Shared by object properties and
/// query / path struct parameters.
pub fn apply_field_rules(schema_ref: &mut SchemaRef, field: &syn::Field, c: &SchemaConstraints) {
    if !c.is_any() {
        apply_garde_rules(schema_ref, &field.ty, &extract_garde_rules(&field.attrs), c);
    }
    if !c.is_empty() {
        apply_constraints_to_schema_ref(schema_ref, c);
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

#[cfg(test)]
mod tests_wire_override;
