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
    garde_rules::{apply_garde_rules, extract_garde_rules},
    schema_attrs::{SchemaConstraints, extract_schema_constraints},
    serde_attrs::{
        SerdeSide, extract_aliases, extract_default, extract_deny_unknown_fields,
        extract_doc_comment, extract_field_codec, extract_field_rename_for, extract_flatten,
        extract_rename_all, extract_rename_all_for, extract_schema_ref_override,
        extract_serde_skip_flags, extract_tag, extract_transparent, rename_field,
        strip_raw_prefix_owned,
    },
    type_schema::parse_type_to_schema_ref,
    wire_override::{current_container_override, field_schema_ref},
};
use crate::schema_macro::type_utils::is_option_type;

mod flatten;

pub(super) use flatten::resolve_type;
use flatten::{FlatPart, flatten_part};

/// Parses a Rust struct into an `OpenAPI` Schema.
///
/// This function extracts:
/// - Field names and types as properties
/// - Required fields (non-`Option` types). In the response view a
///   `#[serde(default)]` field stays present on output; inside
///   `request_view::with_request_view` (the `{Name}Input` view) it is optional
/// - Doc comments as descriptions
/// - Serde attributes (rename, `rename_all`, skip, default, alias, flatten)
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
            let mut fields = collect_named_fields(
                fields_named,
                rename_all.as_deref(),
                container_default,
                known_schemas,
                struct_definitions,
            );
            // `#[serde(tag = "t")]` on a struct: serde's Serialize writes
            // `"t": <serialize name>` before the fields (also through the map
            // path a `flatten` struct uses), so the response view requires
            // it. serde's Deserialize ignores the tag entirely: an absent tag
            // is accepted and a present one is just an unknown key (rejected
            // under `deny_unknown_fields`), so the request view (`{Name}Input`,
            // see `request_view::struct_differs`) leaves it out.
            if !request && let Some(tag) = extract_tag(&struct_item.attrs) {
                let serde_name = extract_field_rename_for(&struct_item.attrs, SerdeSide::Serialize)
                    .unwrap_or_else(|| strip_raw_prefix_owned(struct_item.ident.to_string()));
                fields.properties.insert(
                    tag.clone(),
                    SchemaRef::Inline(Box::new(Schema {
                        r#enum: Some(vec![serde_json::Value::String(serde_name)]),
                        ..Schema::string()
                    })),
                );
                fields.required.insert(0, tag);
            }

            // `deny_unknown_fields` → closed object (see `into_object_schema`
            // for how it combines with `flatten`).
            fields.into_object_schema(
                struct_description,
                extract_deny_unknown_fields(&struct_item.attrs),
            )
        }
        // `struct Pair(A, B)` → `[A, B]` (positions skipped on the current
        // side are not serialized / read, so they are not array items).
        Fields::Unnamed(fields_unnamed) => Schema {
            description: struct_description,
            ..tuple_fields_schema(fields_unnamed, known_schemas, struct_definitions)
        },
        // `struct Marker;` → serde writes (and reads) a unit struct as `null`.
        Fields::Unit => Schema {
            description: struct_description,
            ..Schema::new(SchemaType::Null)
        },
    }
}

/// Fixed-length array of a tuple struct / tuple variant on the side of the
/// view being built: a position skipped on that side
/// (`#[serde(skip)]`, `skip_serializing` in the response view,
/// `skip_deserializing` in the request view) is neither written nor read,
/// and each position honours its `#[schema(value_type)]` / `#[schema(any)]` /
/// serde `with` family (see [`field_schema_ref`]).
pub(super) fn tuple_fields_schema(
    fields: &syn::FieldsUnnamed,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let side = super::request_view::current_side();
    let prefix_items: Vec<SchemaRef> = fields
        .unnamed
        .iter()
        .filter(|field| {
            let flags = extract_serde_skip_flags(&field.attrs);
            !(flags.skip
                || match side {
                    SerdeSide::Serialize => flags.skip_serializing,
                    SerdeSide::Deserialize => flags.skip_deserializing,
                })
        })
        .map(|field| {
            field_schema_ref(
                field,
                &extract_schema_constraints(&field.attrs),
                side,
                known_schemas,
                struct_definitions,
            )
        })
        .collect();
    let len = prefix_items.len();
    Schema {
        prefix_items: Some(prefix_items),
        min_items: Some(len),
        max_items: Some(len),
        ..Schema::new(SchemaType::Array)
    }
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
    /// `#[serde(flatten)]` members (see [`flatten`]).
    flattened: Vec<FlatPart>,
    /// Request view: required fields that also accept `#[serde(alias)]`
    /// names — exactly one name of each group must be present.
    alias_groups: Vec<Vec<String>>,
}

impl NamedFields {
    /// Object schema for these fields, `#[serde(flatten)]` members merged in
    /// (see [`flatten::assemble`]); `deny_unknown_fields` closes it.
    pub(super) fn into_object_schema(
        self,
        description: Option<String>,
        deny_unknown_fields: bool,
    ) -> Schema {
        let constraints = self
            .alias_groups
            .into_iter()
            .map(|names| {
                SchemaRef::Inline(Box::new(Schema {
                    one_of: Some(
                        names
                            .into_iter()
                            .map(|name| {
                                SchemaRef::Inline(Box::new(Schema {
                                    required: Some(vec![name]),
                                    ..Schema::default()
                                }))
                            })
                            .collect(),
                    ),
                    ..Schema::default()
                }))
            })
            .collect();
        flatten::assemble(
            flatten::ObjectParts {
                properties: self.properties,
                required: self.required,
                constraints,
                ..flatten::ObjectParts::default()
            },
            self.flattened,
            description,
            deny_unknown_fields,
        )
    }
}

/// Walk named fields applying every field-level attribute vespera understands:
/// `#[serde(skip)]` (and `skip_serializing` + `skip_deserializing`),
/// `#[serde(flatten)]`, `#[serde(rename)]` / `rename_all` / `alias`, doc
/// comments, `#[schema(...)]` constraints (incl. `any`), and `required`
/// inference (`Option<T>`, `skip_serializing`, and `skip_serializing_if`
/// fields are not required in the response view; `default` fields in the
/// request view).
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
    // dropped, keys use the deserialize name, `#[serde(default)]` (field or
    // container) makes a field optional, and aliases are accepted keys.
    let request = super::request_view::in_request_view();
    // Field-level rename takes precedence over rename_all.
    let side = if request {
        SerdeSide::Deserialize
    } else {
        SerdeSide::Serialize
    };
    for field in &fields_named.named {
        let skip_flags = extract_serde_skip_flags(&field.attrs);
        if skip_flags.fully_skipped() || (request && skip_flags.skip_deserializing) {
            continue;
        }

        if extract_flatten(&field.attrs) {
            out.flattened
                .push(flatten_part(field, side, known_schemas, struct_definitions));
            continue;
        }

        let rust_field_name = field.ident.as_ref().map_or_else(
            || "unknown".to_string(),
            |i| strip_raw_prefix_owned(i.to_string()),
        );
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

        // `#[serde(alias = "..")]`: serde's Deserialize also accepts each
        // alias as the key (a second key for the same field is a "duplicate
        // field" error), so in the request view every alias is a property
        // with the field's schema, and a required field needs exactly one of
        // its names.
        let aliases = if request {
            extract_aliases(&field.attrs)
        } else {
            Vec::new()
        };
        for alias in &aliases {
            out.properties
                .entry(alias.clone())
                .or_insert_with(|| schema_ref.clone());
        }
        if !absent_is_none && !optional {
            if aliases.is_empty() {
                out.required.push(field_name.clone());
            } else {
                let mut names = vec![field_name.clone()];
                names.extend(aliases);
                out.alias_groups.push(names);
            }
        }

        out.properties.insert(field_name, schema_ref);
    }
    out
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

#[cfg(test)]
mod tests_wire_names;

#[cfg(test)]
mod tests_serde_fidelity;
