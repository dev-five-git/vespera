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
    schema_attrs::{SchemaConstraints, SchemaValueType, extract_schema_constraints},
    serde_attrs::{
        SerdeSide, derives_serialize, extract_aliases, extract_default,
        extract_deny_unknown_fields, extract_doc_comment, extract_field_codec,
        extract_field_rename_for, extract_field_rename_owned, extract_flatten, extract_rename_all,
        extract_rename_all_for, extract_schema_ref_override, extract_serde_rename_all,
        extract_serde_skip_flags, extract_tag, extract_transparent, rename_field,
        skips_none_on_output, strip_raw_prefix_owned,
    },
    type_schema::parse_type_to_schema_ref,
    wire_override::{current_container_override, field_schema_ref},
};
use crate::schema_macro::type_utils::{is_option_type, option_inner};

mod flatten;

use flatten::{FlatPart, flatten_part};
pub(super) use flatten::{
    flattened_member, flattened_unit_variants, peel, resolve_reference, resolve_type,
};

/// Parses a Rust struct into an `OpenAPI` Schema.
///
/// This function extracts:
/// - Field names and types as properties
/// - Required fields: every field serde always writes. An `Option` is written
///   as `null`, so it is required in the response view. In the request view
///   (`request_view::with_request_view`, the `{Name}Input` view) a
///   `#[serde(default)]` field, an `Option` field, or a field with
///   `skip_serializing_if` (response only) is optional
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
    // The generic definition's own component: `Tree<T>` inside `Tree<T>`
    // refers back to `Tree` instead of materialising `Tree_T`.
    super::generic_instances::with_definition(
        &struct_item.ident,
        &struct_item.generics,
        &struct_item.attrs,
        &|path: &syn::Path| {
            super::type_schema::scoped_component(path, known_schemas, struct_definitions)
        },
        || parse_struct_body(struct_item, known_schemas, struct_definitions),
    )
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
            ref_path: Some(vespera_core::schema::Reference::schema(&schema_name).ref_path),
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
    let side = super::request_view::current_side();
    let inner_field = match &struct_item.fields {
        Fields::Named(fields_named) if transparent => {
            single_emitted_field(fields_named.named.iter(), side)
        }
        Fields::Unnamed(fields_unnamed) if transparent => {
            single_emitted_field(fields_unnamed.unnamed.iter(), side)
        }
        Fields::Unnamed(fields_unnamed) if fields_unnamed.unnamed.len() == 1 => {
            fields_unnamed.unnamed.first()
        }
        _ => None,
    };
    if let Some(field) = inner_field {
        let schema_ref = positional_field_schema(field, side, known_schemas, struct_definitions);
        return with_description(schema_ref, struct_description);
    }

    match &struct_item.fields {
        Fields::Named(fields_named) => {
            let request = super::request_view::in_request_view();
            let serializes = derives_serialize(&struct_item.attrs);
            // The response view of an owner that derives `Serialize` documents
            // serde's own `rename_all`: a `try_from_multipart` rule names the
            // form fields and never reaches serde's output.
            let rename_all = if request {
                extract_rename_all_for(&struct_item.attrs, SerdeSide::Deserialize)
            } else if serializes {
                extract_serde_rename_all(&struct_item.attrs)
            } else {
                extract_rename_all(&struct_item.attrs)
            };
            let container_default = request && extract_default(&struct_item.attrs).is_some();
            let mut fields = collect_named_fields(
                fields_named,
                rename_all.as_deref(),
                container_default,
                serializes,
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

            // `deny_unknown_fields` (or a strict Multipart derive) → closed
            // object (see `into_object_schema` for how it combines with `flatten`).
            fields.into_object_schema(
                struct_description,
                extract_deny_unknown_fields(&struct_item.attrs)
                    || crate::multipart_impl::rejects_unknown_parts(&struct_item.attrs),
            )
        }
        // `struct Pair(A, B)` → `[A, B]` (positions skipped on the current
        // side are not serialized / read, so they are not array items).
        Fields::Unnamed(fields_unnamed) => Schema {
            description: struct_description,
            ..tuple_fields_schema(
                fields_unnamed,
                extract_default(&struct_item.attrs).is_some(),
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

/// Fixed-length array of a tuple struct / tuple variant on the side of the
/// view being built: a position skipped on that side
/// (`#[serde(skip)]`, `skip_serializing` in the response view,
/// `skip_deserializing` in the request view) is neither written nor read,
/// and each position honours its `#[schema(value_type)]` / `#[schema(any)]` /
/// serde `with` family (see [`field_schema_ref`]). serde reads a missing
/// trailing position from its `#[serde(default)]` (or the container's, for
/// a tuple struct — `container_default`; serde_derive
/// `expr_is_missing_seq`), so the request view only requires the positions
/// up to the last one without a default.
pub(super) fn tuple_fields_schema(
    fields: &syn::FieldsUnnamed,
    container_default: bool,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let side = super::request_view::current_side();
    let emitted: Vec<&syn::Field> = fields
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
        .collect();
    let len = emitted.len();
    // serde_derive leaves out a position whose `skip_serializing_if`
    // predicate holds (and counts it out of the tuple length), so every
    // later element moves one slot to the left.
    let omittable: Vec<bool> = emitted
        .iter()
        .map(|field| {
            side == SerdeSide::Serialize
                && extract_serde_skip_flags(&field.attrs).skip_serializing_if
        })
        .collect();
    let min_items = match side {
        SerdeSide::Serialize => omittable.iter().filter(|omittable| !**omittable).count(),
        SerdeSide::Deserialize if container_default => 0,
        SerdeSide::Deserialize => emitted
            .iter()
            .rposition(|field| extract_default(&field.attrs).is_none())
            .map_or(0, |last| last + 1),
    };
    let schemas: Vec<SchemaRef> = emitted
        .into_iter()
        .map(|field| {
            // The response view of an `Option` serde skips when `None` only
            // ever writes its `Some` value (see `collect_named_fields`).
            let some_only = (side == SerdeSide::Serialize && skips_none_on_output(&field.attrs))
                .then(|| some_value(field, &extract_schema_constraints(&field.attrs)))
                .flatten();
            let schema_ref = match &some_only {
                Some((field, constraints)) => positional_schema_with(
                    field,
                    constraints,
                    side,
                    known_schemas,
                    struct_definitions,
                ),
                None => positional_field_schema(field, side, known_schemas, struct_definitions),
            };
            annotate_field(schema_ref, field, false)
        })
        .collect();
    // Slot `k` holds field `j >= k` when `j - k` omittable positions precede
    // `j`: one schema per possible field (`anyOf` when several can land there).
    let prefix_items = (0..len)
        .map(|slot| {
            let mut candidates = (slot..len)
                .filter(|&field| {
                    field - slot
                        <= omittable[..field]
                            .iter()
                            .filter(|omittable| **omittable)
                            .count()
                })
                .map(|field| schemas[field].clone())
                .collect::<Vec<_>>();
            if candidates.len() == 1 {
                candidates.remove(0)
            } else {
                SchemaRef::Inline(Box::new(Schema {
                    any_of: Some(candidates),
                    ..Schema::default()
                }))
            }
        })
        .collect();
    Schema {
        prefix_items: Some(prefix_items),
        min_items: Some(min_items),
        max_items: Some(len),
        ..Schema::new(SchemaType::Array)
    }
}

/// serde's transparent field on `side` (serde_derive `check_transparent` /
/// `allow_transparent`): the ONLY field that is not `PhantomData` and is not
/// skipped on that side (`skip` / `skip_serializing` when writing;
/// `skip` / `skip_deserializing` / a field `default` when reading).
pub fn single_emitted_field<'a>(
    fields: impl Iterator<Item = &'a syn::Field>,
    side: SerdeSide,
) -> Option<&'a syn::Field> {
    let mut emitted = fields.filter(|field| {
        if is_phantom_data(&field.ty) {
            return false;
        }
        let flags = extract_serde_skip_flags(&field.attrs);
        !flags.skip
            && match side {
                SerdeSide::Serialize => !flags.skip_serializing,
                SerdeSide::Deserialize => {
                    !flags.skip_deserializing && extract_default(&field.attrs).is_none()
                }
            }
    });
    let first = emitted.next()?;
    emitted.next().is_none().then_some(first)
}

/// `PhantomData<..>` by its last path segment (serde's own test).
fn is_phantom_data(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Group(group) => is_phantom_data(&group.elem),
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "PhantomData"),
        _ => false,
    }
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
    /// Request view: fields that also accept `#[serde(alias)]` names, with
    /// whether the field is required — exactly one (required) or at most one
    /// (optional) name of each group may be present.
    alias_groups: Vec<(Vec<String>, bool)>,
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
            .map(|(names, required)| {
                if !required {
                    return at_most_one(&names);
                }
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

/// `{not: {anyOf: [{required: [a, b]}, ..]}}` over every pair of `names`:
/// serde rejects a second name of the same field (`duplicate_field`), even
/// when the field may be absent.
fn at_most_one(names: &[String]) -> SchemaRef {
    let mut pairs = Vec::new();
    for (index, first) in names.iter().enumerate() {
        for second in &names[index + 1..] {
            pairs.push(SchemaRef::Inline(Box::new(Schema {
                required: Some(vec![first.clone(), second.clone()]),
                ..Schema::default()
            })));
        }
    }
    let both = if pairs.len() == 1 {
        pairs.remove(0)
    } else {
        SchemaRef::Inline(Box::new(Schema {
            any_of: Some(pairs),
            ..Schema::default()
        }))
    };
    SchemaRef::Inline(Box::new(Schema {
        not: Some(both),
        ..Schema::default()
    }))
}

/// Walk named fields applying every field-level attribute vespera understands:
/// `#[serde(skip)]` (and `skip_serializing` + `skip_deserializing`),
/// `#[serde(flatten)]`, `#[serde(rename)]` / `rename_all` / `alias`, doc
/// comments, `#[schema(...)]` constraints (incl. `any`), and `required`
/// inference (`skip_serializing_if` fields are not required in the response
/// view; `Option<T>` and `default` fields in the request view). A field one
/// side never touches is absent from that view (`skip_serializing` from the
/// response view, `skip_deserializing` from the request view).
pub(super) fn collect_named_fields(
    fields_named: &syn::FieldsNamed,
    rename_all: Option<&str>,
    container_default: bool,
    owner_serializes: bool,
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
        if skip_flags.fully_skipped()
            || (request && skip_flags.skip_deserializing)
            || (!request && skip_flags.skip_serializing)
        {
            continue;
        }

        if extract_flatten(&field.attrs) {
            // Response view: a `skip_serializing_if` member may be left out
            // of the output entirely, so its keys are never required.
            let omittable = !request && skip_flags.skip_serializing_if;
            out.flattened.push(flatten_part(
                field,
                side,
                omittable,
                known_schemas,
                struct_definitions,
            ));
            continue;
        }

        let rust_field_name = field.ident.as_ref().map_or_else(
            || "unknown".to_string(),
            |i| strip_raw_prefix_owned(i.to_string()),
        );
        let field_name = extract_field_rename_owned(&field.attrs, side, owner_serializes)
            .unwrap_or_else(|| rename_field(&rust_field_name, rename_all));

        // `#[schema(any)]` / `#[schema(value_type = T)]` / serde `with`
        // replace the Rust type's schema (see `wire_override`).
        let constraints = extract_schema_constraints(&field.attrs);
        // Response view of an `Option` serde skips when `None`: only a
        // `Some` is ever written, so its value is documented (no `null`).
        let some_only = (!request && skips_none_on_output(&field.attrs))
            .then(|| some_value(field, &constraints))
            .flatten();
        let (field, constraints) = some_only
            .as_ref()
            .map_or((field, &constraints), |(field, constraints)| {
                (field, constraints)
            });
        let mut schema_ref =
            field_schema_ref(field, constraints, side, known_schemas, struct_definitions);

        // Doc comment → description; Rust `#[deprecated]` → `deprecated`;
        // `skip_deserializing` (output only) → `readOnly` in the response
        // view. A `$ref` is wrapped in `allOf` so the annotations can sit
        // beside it; explicit `#[schema(read_only, ..)]` constraints applied
        // below still win.
        let read_only = !request && skip_flags.skip_deserializing;
        schema_ref = annotate_field(schema_ref, field, read_only);

        apply_field_rules(&mut schema_ref, field, constraints);

        // Required in the response view unless a `skip_serializing_if`
        // predicate may leave the key out: serde always writes the rest, an
        // `Option` as `null` (its schema is nullable). In the request view a
        // field is optional with `#[serde(default)]` (field or container),
        // since serde fills a missing defaulted field in (the Multipart
        // derive's `#[form_data(default)]` likewise), and an `Option`, which
        // serde fills with `None` — unless a custom `deserialize_with` reads
        // it, which makes it mandatory.
        let optional = if request {
            container_default
                || extract_default(&field.attrs).is_some()
                || crate::multipart_impl::has_form_data_default(&field.attrs)
        } else {
            skip_flags.skip_serializing_if
        };
        // Syntactic: only a written `Option<..>` (or a path ending in it) counts.
        // A type alias of `Option` (`type Nick = Option<String>`) is not
        // resolved, so its field stays listed as required although serde
        // accepts it missing (a documented limit, not a spec-accuracy promise).
        let absent_is_none =
            request && is_option_type(&field.ty) && !extract_field_codec(&field.attrs).deserialize;

        // `#[serde(alias = "..")]`: serde's Deserialize also accepts each
        // alias as the key (a second key for the same field is a "duplicate
        // field" error), so in the request view every alias is a property
        // with the field's schema, and a required field needs exactly one of
        // its names.
        // serde_derive keeps a field's accepted names in a set: an alias equal
        // to the wire name, or repeated, is that one name.
        let mut aliases = if request {
            extract_aliases(&field.attrs)
        } else {
            Vec::new()
        };
        let mut seen = HashSet::from([field_name.clone()]);
        aliases.retain(|alias| seen.insert(alias.clone()));
        for alias in &aliases {
            out.properties
                .entry(alias.clone())
                .or_insert_with(|| schema_ref.clone());
        }
        let required = !absent_is_none && !optional;
        if aliases.is_empty() {
            if required {
                out.required.push(field_name.clone());
            }
        } else {
            let mut names = vec![field_name.clone()];
            names.extend(aliases);
            out.alias_groups.push((names, required));
        }

        out.properties.insert(field_name, schema_ref);
    }
    out
}

/// `field` documented as the `T` of its `Option<T>` (or of a
/// `#[schema(value_type = Option<T>)]`), or `None` when it is no `Option`.
fn some_value(
    field: &syn::Field,
    constraints: &SchemaConstraints,
) -> Option<(syn::Field, SchemaConstraints)> {
    match &constraints.value_type {
        Some(value_type) => Some((
            field.clone(),
            SchemaConstraints {
                value_type: Some(SchemaValueType(option_inner(&value_type.0)?.clone())),
                ..constraints.clone()
            },
        )),
        None => Some((
            syn::Field {
                ty: option_inner(&field.ty)?.clone(),
                ..field.clone()
            },
            constraints.clone(),
        )),
    }
}

/// A field's doc comment → `description`, Rust `#[deprecated]` →
/// `deprecated`, plus `readOnly` when set. A `$ref` is wrapped in `allOf`
/// so the annotations can sit beside it.
fn annotate_field(schema_ref: SchemaRef, field: &syn::Field, read_only: bool) -> SchemaRef {
    let doc = extract_doc_comment(&field.attrs);
    let deprecated = field
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("deprecated"));
    if !(doc.is_some() || deprecated || read_only) {
        return schema_ref;
    }
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
    SchemaRef::Inline(schema)
}

/// The schema of a positional field (a tuple-struct position, a newtype
/// struct / variant payload, a transparent struct's field) on `side`: its
/// wire type (see [`field_schema_ref`]) with its `#[garde(..)]` rules and
/// `#[schema(..)]` constraints applied, like a named field's.
pub(super) fn positional_field_schema(
    field: &syn::Field,
    side: SerdeSide,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    let constraints = extract_schema_constraints(&field.attrs);
    positional_schema_with(field, &constraints, side, known_schemas, struct_definitions)
}

/// [`positional_field_schema`] with the field's `constraints` given (a
/// response view may document the `Some` value of a skipped `Option`).
fn positional_schema_with(
    field: &syn::Field,
    constraints: &SchemaConstraints,
    side: SerdeSide,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    let mut schema_ref =
        field_schema_ref(field, constraints, side, known_schemas, struct_definitions);
    if !constraints.any_decides() {
        apply_garde_rules(
            &mut schema_ref,
            &field.ty,
            &extract_garde_rules(&field.attrs),
            constraints,
        );
    }
    // Only keywords land on the schema (`value_type` / `any` / `dive` are
    // already reflected): a bare payload `$ref` stays a `$ref`.
    let keywords = SchemaConstraints {
        dive: None,
        any: None,
        value_type: None,
        ..constraints.clone()
    };
    if !keywords.is_empty() {
        apply_constraints_to_schema_ref(&mut schema_ref, &keywords);
    }
    schema_ref
}

/// Document a field's validation rules on its schema: the `#[garde(...)]`
/// rules a `garde::Validate` derive enforces (see [`super::garde_rules`]),
/// then the field-level `#[schema(min_length=..., pattern=..., ...)]`
/// constraints `c` (explicit — they win). Shared by object properties and
/// query / path struct parameters.
pub fn apply_field_rules(schema_ref: &mut SchemaRef, field: &syn::Field, c: &SchemaConstraints) {
    if !c.any_decides() {
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
    // A bare flag makes the bound already documented (a garde `range`, the
    // type's own `minimum: 0`) exclusive.
    if c.exclusive_minimum == Some(true) {
        schema.exclusive_minimum = schema.minimum;
    }
    if c.exclusive_maximum == Some(true) {
        schema.exclusive_maximum = schema.maximum;
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

#[cfg(test)]
mod tests_transparent;

#[cfg(test)]
mod tests_ref_escape;

#[cfg(test)]
mod tests_option_output;
