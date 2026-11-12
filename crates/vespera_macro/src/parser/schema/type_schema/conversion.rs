//! Type to JSON Schema conversion for `OpenAPI` generation.
//!
//! This module handles the conversion of Rust types (as parsed by syn)
//! into OpenAPI-compatible JSON Schema references and inline schemas.

use std::{
    borrow::Borrow,
    cell::Cell,
    collections::{HashMap, HashSet},
    hash::Hash,
    rc::Rc,
};

use syn::Type;
use vespera_core::schema::{AdditionalProperties, Reference, Schema, SchemaRef, SchemaType};

/// Maximum recursion depth for type-to-schema conversion.
/// Prevents stack overflow from deeply nested or circular type references.
pub(super) const MAX_SCHEMA_RECURSION_DEPTH: usize = 32;

thread_local! {
    pub(super) static SCHEMA_RECURSION_DEPTH: Cell<usize> = const { Cell::new(0) };
}

use super::super::{
    generics::substitute_type,
    serde_attrs::{capitalize_first, extract_schema_name_from_entity, extract_schema_ref_override},
    struct_schema::parse_struct_to_schema,
};

/// Parse a known schema's definition into a shared `syn::ItemStruct`.
///
/// MUST NOT memoise the parsed AST in a `thread_local` (or any storage that
/// outlives the macro invocation).  A `syn::ItemStruct` parsed during real
/// proc-macro expansion holds bridge-backed `proc_macro2` tokens whose `Drop`
/// calls into the proc-macro bridge; if such a value survived in TLS until the
/// proc-macro server thread exits, that drop would run with NO active expansion
/// and abort the compile ("procedural macro API is used outside of a procedural
/// macro" -> STATUS_STACK_BUFFER_OVERRUN).  Every AST returned here is therefore
/// dropped within the same invocation that parsed it.
fn parse_struct_def(def: &str) -> Option<Rc<syn::ItemStruct>> {
    syn::parse_str::<syn::ItemStruct>(def).ok().map(Rc::new)
}

/// Whether `ident` names a type that this crate already knows how to map to
/// an OpenAPI schema — Rust primitives, `String`/`str`, the chrono/time
/// date-time set, `Uuid`/`Decimal`/`StatusCode`, `FieldData`/`NamedTempFile`
/// (multipart binary), and the framework wrapper idents (`Vec`, `Option`,
/// `Box`, `HashSet`, `BTreeSet`, `HashMap`, `BTreeMap`, `HasOne`, `HasMany`,
/// `BelongsTo`, `Result`, `Json`, `Path`, `Query`, `Header`).
///
/// This is the **single source of truth** consulted by both:
///
/// 1. [`parse_type_impl`] below — to decide whether the type maps to a
///    specific schema arm (covered by the `matches!` arms at the bottom of
///    the match) or falls through to the unknown-fallback (`SchemaRef::Inline
///    (Object)`).
/// 2. `#[derive(Schema)]` codegen in `schema_impl::process_derive_schema` —
///    to decide whether a field's leaf type needs a compile-time
///    `T: ::vespera::Schema` assertion (only NON-builtin leaves do).
///
/// Adding a new builtin to the match arms below MUST also add it here, and
/// vice-versa — the [`tests::is_builtin_openapi_type_matches_conversion_arms`]
/// test pins the two lists in sync.
#[must_use]
pub const fn is_builtin_openapi_type(ident: &str) -> bool {
    matches!(
        ident.as_bytes(),
        // Signed integers
        b"i8" | b"i16" | b"i32" | b"i64" | b"i128" | b"isize" | b"StatusCode"
        // Unsigned integers
        | b"u8" | b"u16" | b"u32" | b"u64" | b"u128" | b"usize"
        // Floats
        | b"f32" | b"f64"
        // Decimal (rust_decimal / sea_orm re-export — serialized as string)
        | b"Decimal"
        // Bool / char / Uuid
        | b"bool" | b"char" | b"Uuid"
        // String / str
        | b"String" | b"str"
        // Date-time set (chrono + time crates + sea_orm re-exports)
        | b"DateTime"
        | b"NaiveDateTime"
        | b"DateTimeWithTimeZone"
        | b"DateTimeUtc"
        | b"DateTimeLocal"
        | b"OffsetDateTime"
        | b"PrimitiveDateTime"
        | b"NaiveDate" | b"Date"
        | b"NaiveTime" | b"Time"
        | b"Duration"
        // File upload (vespera::multipart / tempfile)
        | b"FieldData" | b"NamedTempFile"
        // Wrappers / std container types handled BEFORE the leaf match
        // (transparent for schema OR mapped to array/object via generic args).
        | b"Vec" | b"VecDeque" | b"LinkedList" | b"HashSet" | b"BTreeSet" | b"Option"
        | b"Box" | b"Arc" | b"Rc" | b"Cow"
        | b"HashMap" | b"BTreeMap"
        // std::num::NonZero integers (generic + per-width aliases)
        | b"NonZero"
        | b"NonZeroI8" | b"NonZeroI16" | b"NonZeroI32" | b"NonZeroI64" | b"NonZeroI128"
        | b"NonZeroIsize"
        | b"NonZeroU8" | b"NonZeroU16" | b"NonZeroU32" | b"NonZeroU64" | b"NonZeroU128"
        | b"NonZeroUsize"
        // SeaORM relation wrappers
        | b"HasOne" | b"HasMany" | b"BelongsTo"
        // Result + axum extractor wrappers that have no schema themselves
        | b"Result" | b"Json" | b"Path" | b"Query" | b"Header"
    )
}

/// Inline schema built from `base` with an OpenAPI `format` string attached.
///
/// Shared body of [`integer_with_format`], [`number_with_format`] and
/// [`string_with_format`], which differ only in the `base` schema they pass.
fn schema_with_format(base: Schema, format: &str) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        format: Some(format.to_string()),
        ..base
    }))
}

/// Inline integer schema with an OpenAPI format string.
fn integer_with_format(format: &str) -> SchemaRef {
    schema_with_format(Schema::integer(), format)
}

/// Inline number schema with an OpenAPI format string.
fn number_with_format(format: &str) -> SchemaRef {
    schema_with_format(Schema::number(), format)
}

/// Inline string schema with an OpenAPI format string.
fn string_with_format(format: &str) -> SchemaRef {
    schema_with_format(Schema::string(), format)
}

/// Check if a type is a primitive Rust type that maps directly to a JSON Schema type.
pub fn is_primitive_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            if path.segments.len() == 1 {
                let ident = path.segments[0].ident.to_string();
                ident == "str"
                    || crate::schema_macro::type_utils::PRIMITIVE_TYPE_NAMES
                        .contains(&ident.as_str())
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Converts a Rust type to an `OpenAPI` `SchemaRef` with depth-guarded recursion.
///
/// This is the single entry point for type-to-schema conversion. Handles:
/// - Primitive types (i32, String, bool, etc.)
/// - Generic wrappers (Vec, Option, Box)
/// - `SeaORM` relations (`HasOne`, `HasMany`)
/// - Map types (`HashMap`, `BTreeMap`)
/// - Date/time types (`DateTime`, `NaiveDate`, etc.)
/// - Known schema references
/// - Generic type instantiation
pub fn parse_type_to_schema_ref(
    ty: &Type,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    SCHEMA_RECURSION_DEPTH.with(|depth| {
        let current = depth.get();
        if current >= MAX_SCHEMA_RECURSION_DEPTH {
            return SchemaRef::Inline(Box::new(Schema {
                description: Some(format!(
                    "Schema generation stopped after reaching recursion depth limit ({MAX_SCHEMA_RECURSION_DEPTH}) for `{}`",
                    quote::quote!(#ty)
                )),
                ..Schema::new(SchemaType::Object)
            }));
        }
        depth.set(current + 1);
        let result = parse_type_impl(ty, known_schemas, struct_definitions);
        depth.set(current);
        result
    })
}

/// Core type-to-schema logic (called within depth guard).
#[allow(clippy::too_many_lines)]
fn parse_type_impl(
    ty: &Type,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> SchemaRef {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            // Get the last segment as the type name (handles paths like crate::TestStruct).
            // A segment-less path falls back to the default inline object schema instead
            // of panicking the proc macro.
            let Some(segment) = path.segments.last() else {
                return SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)));
            };
            let ident_str = segment.ident.to_string();

            // Handle generic types
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                match ident_str.as_str() {
                    // Box<T> / Arc<T> / Rc<T> / Cow<'a, T> -> T's schema: smart
                    // pointers are transparent for serde (and so for the schema).
                    // `first_type_arg` skips `Cow`'s leading lifetime argument.
                    "Box" | "Arc" | "Rc" | "Cow" => {
                        if let Some(inner_ty) = first_type_arg(args) {
                            return parse_type_to_schema_ref(
                                inner_ty,
                                known_schemas,
                                struct_definitions,
                            );
                        }
                    }
                    // std::num::NonZero<T> -> T's integer schema; unsigned
                    // widths additionally get `minimum: 1`.
                    "NonZero" => {
                        if let Some(inner_ty) = first_type_arg(args) {
                            let mut inner_schema = parse_type_to_schema_ref(
                                inner_ty,
                                known_schemas,
                                struct_definitions,
                            );
                            if let SchemaRef::Inline(schema) = &mut inner_schema
                                && is_unsigned_int_type(inner_ty)
                            {
                                schema.minimum = Some(1.0);
                            }
                            return inner_schema;
                        }
                    }
                    "Vec" | "VecDeque" | "LinkedList" | "HashSet" | "BTreeSet" | "Option" => {
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                            let inner_schema = parse_type_to_schema_ref(
                                inner_ty,
                                known_schemas,
                                struct_definitions,
                            );
                            if matches!(ident_str.as_str(), "Vec" | "VecDeque" | "LinkedList") {
                                return SchemaRef::Inline(Box::new(Schema::array(inner_schema)));
                            }
                            if ident_str == "HashSet" || ident_str == "BTreeSet" {
                                let mut schema = Schema::array(inner_schema);
                                schema.unique_items = Some(true);
                                return SchemaRef::Inline(Box::new(schema));
                            }
                            // Option<T> -> nullable schema
                            match inner_schema {
                                SchemaRef::Inline(mut schema) => {
                                    schema.nullable = Some(true);
                                    return SchemaRef::Inline(schema);
                                }
                                SchemaRef::Ref(reference) => {
                                    // Wrap reference in an inline schema to attach nullable flag
                                    return SchemaRef::Inline(Box::new(
                                        Schema::nullable_reference(reference.ref_path),
                                    ));
                                }
                            }
                        }
                    }
                    // SeaORM relation types: convert Entity to Schema reference
                    "HasOne" => {
                        // HasOne<Entity> -> nullable reference to corresponding Schema
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
                            && let Some(schema_name) = extract_schema_name_from_entity(inner_ty)
                        {
                            return SchemaRef::Inline(Box::new(Schema::nullable_reference(
                                format!("#/components/schemas/{schema_name}"),
                            )));
                        }
                        // Fallback: generic object
                        return SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)));
                    }
                    "HasMany" => {
                        // HasMany<Entity> -> array of references to corresponding Schema
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
                            && let Some(schema_name) = extract_schema_name_from_entity(inner_ty)
                        {
                            let inner_ref = SchemaRef::Ref(Reference::new(format!(
                                "#/components/schemas/{schema_name}"
                            )));
                            return SchemaRef::Inline(Box::new(Schema::array(inner_ref)));
                        }
                        // Fallback: array of generic objects
                        return SchemaRef::Inline(Box::new(Schema::array(SchemaRef::Inline(
                            Box::new(Schema::new(SchemaType::Object)),
                        ))));
                    }
                    "HashMap" | "BTreeMap" => {
                        // HashMap<K, V> or BTreeMap<K, V> -> object with additionalProperties
                        // K is typically String, we use V as the value type
                        if args.args.len() >= 2
                            && let (
                                Some(syn::GenericArgument::Type(_key_ty)),
                                Some(syn::GenericArgument::Type(value_ty)),
                            ) = (args.args.get(0), args.args.get(1))
                        {
                            let value_schema = parse_type_to_schema_ref(
                                value_ty,
                                known_schemas,
                                struct_definitions,
                            );
                            // Carry the value schema directly as a typed
                            // `AdditionalProperties::Schema` — no
                            // `SchemaRef -> serde_json::Value` round-trip
                            // (CORE-04).  Untagged serialization is
                            // byte-identical to the prior JSON form.
                            return SchemaRef::Inline(Box::new(Schema {
                                schema_type: Some(SchemaType::Object),
                                additional_properties: Some(AdditionalProperties::Schema(
                                    value_schema,
                                )),
                                ..Schema::object()
                            }));
                        }
                    }
                    _ => {}
                }
            }

            // Handle primitive types
            // For standard OpenAPI format types (i32, i64, f32, f64), use `format`
            // per the OAS 3.1 Data Type Format spec. For non-standard types, fall
            // back to `minimum`/`maximum` constraints.
            //
            // The primitive table returns `Option<SchemaRef>` so the unknown-type
            // fallback can MOVE `ident_str` (rather than `.clone()` it) once the
            // outer match borrow ends — a duplicate-allocation cleanup. Token
            // output is byte-identical: every primitive arm still produces the
            // same `SchemaRef`, and the fallback path consumes the same
            // `resolved_name` string.
            if is_json_value_path(path, known_schemas) {
                // `serde_json::Value` accepts any JSON value → `{}`.
                return SchemaRef::Inline(Box::default());
            }
            let primitive_result: Option<SchemaRef> = match ident_str.as_str() {
                // Signed integers: use OpenAPI format registry
                // https://spec.openapis.org/registry/format/index.html
                // (std::num::NonZeroI* share the base wire type.)
                "i8" | "NonZeroI8" => Some(integer_with_format("int8")),
                "i16" | "NonZeroI16" => Some(integer_with_format("int16")),
                "i32" | "NonZeroI32" => Some(integer_with_format("int32")),
                "i64" | "NonZeroI64" => Some(integer_with_format("int64")),
                // Unsigned integers: use OpenAPI format registry
                "u8" => Some(integer_with_format("uint8")),
                "u16" => Some(integer_with_format("uint16")),
                "u32" => Some(integer_with_format("uint32")),
                "u64" => Some(integer_with_format("uint64")),
                // i128, isize, StatusCode: no standard format in the registry
                "i128" | "isize" | "StatusCode" | "NonZeroI128" | "NonZeroIsize" => {
                    Some(SchemaRef::Inline(Box::new(Schema::integer())))
                }
                // u128, usize: unsigned with no standard format — use minimum: 0
                "u128" | "usize" => Some(SchemaRef::Inline(Box::new(Schema {
                    minimum: Some(0.0),
                    ..Schema::integer()
                }))),
                "f32" => Some(number_with_format("float")),
                "f64" => Some(number_with_format("double")),
                // `rust_decimal` serializes `Decimal` as a JSON *string* (to
                // preserve precision), so the wire type is string, not number.
                "Decimal" => Some(string_with_format("decimal")),
                "bool" => Some(SchemaRef::Inline(Box::new(Schema::boolean()))),
                "char" => Some(string_with_format("char")),
                "Uuid" => Some(string_with_format("uuid")),
                "String" | "str" => Some(SchemaRef::Inline(Box::new(Schema::string()))),
                // Date-time types from chrono and time crates
                "DateTime"
                | "NaiveDateTime"
                | "DateTimeWithTimeZone"
                | "DateTimeUtc"
                | "DateTimeLocal"
                | "OffsetDateTime"
                | "PrimitiveDateTime" => Some(string_with_format("date-time")),
                "NaiveDate" | "Date" => Some(string_with_format("date")),
                "NaiveTime" | "Time" => Some(string_with_format("time")),
                "Duration" => Some(duration_schema(path)),
                // File upload types (vespera::multipart / tempfile)
                // FieldData<NamedTempFile> → string with binary format
                "FieldData" | "NamedTempFile" => Some(string_with_format("binary")),
                // std::num::NonZeroU* can never be 0, so they carry `minimum: 1`
                // (signed NonZeroI* share the base integer arms above).
                "NonZeroU8" => Some(non_zero_unsigned(Some("uint8"))),
                "NonZeroU16" => Some(non_zero_unsigned(Some("uint16"))),
                "NonZeroU32" => Some(non_zero_unsigned(Some("uint32"))),
                "NonZeroU64" => Some(non_zero_unsigned(Some("uint64"))),
                "NonZeroU128" | "NonZeroUsize" => Some(non_zero_unsigned(None)),
                // Standard library types that should not be referenced
                // Note: HashMap and BTreeMap are handled above in generic types
                "Vec" | "VecDeque" | "LinkedList" | "HashSet" | "BTreeSet" | "Option" | "Box"
                | "Arc" | "Rc" | "Cow" | "NonZero" | "Result" | "Json" | "Path" | "Query"
                | "Header" => {
                    // These are not schema types, return object schema
                    Some(SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object))))
                }
                _ => None,
            };
            if let Some(result) = primitive_result {
                return result;
            }

            // Unknown-type fallback: check if this is a known schema (struct
            // with Schema derive). `ident_str` is no longer borrowed by the
            // outer match — MOVE it into `type_name` (was `ident_str.clone()`).
            let type_name = ident_str;

            // For paths like `module::Schema`, try to find the schema name
            // by checking if there's a schema named `ModuleSchema` or `ModuleNameSchema`
            let resolved_name = if type_name == "Schema" && path.segments.len() > 1 {
                // Get the parent module name (e.g., "user" from "crate::models::user::Schema")
                let parent_segment = &path.segments[path.segments.len() - 2];
                let parent_name = parent_segment.ident.to_string();

                // Try PascalCase version: "user" -> "UserSchema"
                // Rust identifiers are guaranteed non-empty
                let pascal_name = format!("{}Schema", capitalize_first(&parent_name));

                if known_schemas.contains(pascal_name.as_str()) {
                    pascal_name
                } else {
                    // Try lowercase version: "userSchema"
                    let lower_name = format!("{parent_name}Schema");
                    if known_schemas.contains(lower_name.as_str()) {
                        lower_name
                    } else {
                        type_name
                    }
                }
            } else {
                type_name
            };

            if known_schemas.contains(resolved_name.as_str()) {
                // Parse the struct definition lazily, ONLY when the parsed AST
                // will actually be consumed downstream.  `parsed_def` feeds two
                // sites: the `#[schema(ref = ...)]` override check below and
                // the generic-substitution branch.  In the common case (a
                // non-generic field referencing a schema whose source carries
                // no `#[schema(...)]` attribute) the AST is dropped unused, so
                // the full `syn::parse_str::<ItemStruct>` tokenise+parse pass
                // is pure waste.  A byte-scan over the cached
                // `quote!`-stringified definition (`extract_schema_ref_override`
                // only fires on `#[schema(...)]`, which `quote!` always emits
                // as the substring `[schema`) is enough to short-circuit that
                // case; the generic branch is gated on `is_generic` anyway.
                // Token output is byte-identical: when both gates miss, the
                // arm now skips straight to `SchemaRef::Ref(...)`, which is
                // exactly the value the unconditional-parse path computed via
                // `parsed_def -> None override -> not generic -> fallback`.
                let def_str = struct_definitions
                    .get(resolved_name.as_str())
                    .map(AsRef::as_ref);
                let is_generic =
                    matches!(&segment.arguments, syn::PathArguments::AngleBracketed(_));
                let maybe_has_schema_attr = def_str.is_some_and(|d| d.contains("[schema"));
                let parsed_def = if is_generic || maybe_has_schema_attr {
                    def_str.and_then(parse_struct_def)
                } else {
                    None
                };

                if let Some(parsed_struct) = &parsed_def
                    && let Some((schema_name, nullable)) =
                        extract_schema_ref_override(&parsed_struct.attrs)
                {
                    return SchemaRef::Inline(Box::new(Schema {
                        ref_path: Some(format!("#/components/schemas/{schema_name}")),
                        schema_type: None,
                        nullable: nullable.then_some(true),
                        ..Schema::new(SchemaType::Object)
                    }));
                }

                // Check if this is a generic type with type parameters
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    // This is a concrete generic type like GenericStruct<String>
                    // Inline the schema by substituting generic parameters with concrete types
                    if let Some(parsed_rc) = parsed_def {
                        // Clone the memoised AST before mutating it for generic
                        // substitution (cheaper than the prior re-parse).
                        let mut parsed = (*parsed_rc).clone();
                        // Extract generic parameter names from the struct definition
                        let generic_params: Vec<String> = parsed
                            .generics
                            .params
                            .iter()
                            .filter_map(|param| {
                                if let syn::GenericParam::Type(type_param) = param {
                                    Some(type_param.ident.to_string())
                                } else {
                                    None
                                }
                            })
                            .collect();

                        // Extract concrete type arguments
                        let concrete_types: Vec<&Type> = args
                            .args
                            .iter()
                            .filter_map(|arg| {
                                if let syn::GenericArgument::Type(ty) = arg {
                                    Some(ty)
                                } else {
                                    None
                                }
                            })
                            .collect();

                        // Substitute generic parameters with concrete types in all fields
                        if generic_params.len() == concrete_types.len() {
                            // (named AND tuple fields — tuple structs now map
                            // to their inner / prefixItems schema).
                            for field in &mut parsed.fields {
                                field.ty =
                                    substitute_type(&field.ty, &generic_params, &concrete_types);
                            }

                            // Remove generics from the struct (it's now concrete)
                            parsed.generics.params.clear();
                            parsed.generics.where_clause = None;

                            // Inline the substituted struct, unless it
                            // contains itself — then it becomes the named
                            // component `Base_Arg..` (see `generic_instances`).
                            let name = super::generic_instances::instance_name(
                                &component_name(resolved_name, def_str),
                                concrete_types.iter().copied(),
                            );
                            return super::generic_instances::inline_or_reference(&name, || {
                                parse_struct_to_schema(&parsed, known_schemas, struct_definitions)
                            });
                        }
                    }
                }
                // Non-generic type or generic without parameters - use reference
                SchemaRef::Ref(Reference::schema(&component_name(resolved_name, def_str)))
            } else {
                // For unknown custom types, return object schema instead of reference
                // This prevents creating invalid references to non-existent schemas
                SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
            }
        }
        Type::Reference(type_ref) => {
            // Handle &T, &mut T, etc. — goes through depth guard via public entry point
            parse_type_to_schema_ref(&type_ref.elem, known_schemas, struct_definitions)
        }
        // () unit type → null (e.g. Json<()> serializes to JSON null)
        Type::Tuple(tuple) if tuple.elems.is_empty() => {
            SchemaRef::Inline(Box::new(Schema::new(SchemaType::Null)))
        }
        // (A, B, ...) → fixed-length array (serde serializes tuples as arrays)
        Type::Tuple(tuple) => SchemaRef::Inline(Box::new(tuple_array_schema(
            tuple.elems.iter(),
            known_schemas,
            struct_definitions,
        ))),
        // [T; N] → array of T; a literal N pins minItems/maxItems.
        Type::Array(array) => {
            let items = parse_type_to_schema_ref(&array.elem, known_schemas, struct_definitions);
            let len = match &array.len {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(int),
                    ..
                }) => int.base10_parse::<usize>().ok(),
                _ => None,
            };
            SchemaRef::Inline(Box::new(Schema {
                min_items: len,
                max_items: len,
                ..Schema::array(items)
            }))
        }
        // [T] (reached through `&[T]`) → array of T
        Type::Slice(slice) => SchemaRef::Inline(Box::new(Schema::array(parse_type_to_schema_ref(
            &slice.elem,
            known_schemas,
            struct_definitions,
        )))),
        Type::Paren(paren) => {
            parse_type_to_schema_ref(&paren.elem, known_schemas, struct_definitions)
        }
        Type::Group(group) => {
            parse_type_to_schema_ref(&group.elem, known_schemas, struct_definitions)
        }
        _ => SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object))),
    }
}

/// The component name a reference to the known type `ident` must use: the
/// `#[schema(name = "...")]` of its definition when it carries one (the Rust
/// ident is only an alias then), otherwise `ident` itself.
fn component_name(ident: String, definition: Option<&str>) -> String {
    definition
        .filter(|def| def.contains("[schema") && def.contains("name"))
        .and_then(|def| syn::parse_str::<syn::DeriveInput>(def).ok())
        .and_then(|input| crate::schema_impl::extract_schema_name_attr(&input.attrs))
        .unwrap_or(ident)
}

/// Fixed-length JSON array for a Rust tuple / tuple struct / tuple variant:
/// `prefixItems` (one schema per position) with `minItems == maxItems == len`.
pub fn tuple_array_schema<'t>(
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

/// First *type* argument of `<...>` (skips lifetimes, e.g. `Cow<'a, str>`).
fn first_type_arg(args: &syn::AngleBracketedGenericArguments) -> Option<&Type> {
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

/// `true` for the bare unsigned primitive integer idents (`u8`..`u128`, `usize`).
fn is_unsigned_int_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(type_path)
    if type_path.path.segments.last().is_some_and(|seg| {
        matches!(seg.ident.to_string().as_str(), "u8" | "u16" | "u32" | "u64" | "u128" | "usize")
    }))
}

/// `serde_json::Value` — any path ending in `Value` that goes through a
/// `serde_json` segment, or a bare `Value` that is not a known component
/// schema (a user `#[derive(Schema)] struct Value` keeps its `$ref`).
fn is_json_value_path(
    path: &syn::Path,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
) -> bool {
    let segments = &path.segments;
    segments.last().is_some_and(|last| last.ident == "Value")
        && if segments.len() == 1 {
            !known_schemas.contains("Value")
        } else {
            segments.iter().any(|segment| segment.ident == "serde_json")
        }
}

/// Schema of a `Duration`, chosen from how its path is spelled.
///
/// - bare `Duration`, `std::time::Duration`, `core::time::Duration`: serde's
///   std impl serializes `{"secs": u64, "nanos": u32}` (nanos < 1e9). A bare
///   `Duration` is assumed to be the std type — `chrono::Duration` has no
///   serde impl at all, so it cannot be a bare serialized field.
/// - any other qualified `Duration` (`time::Duration`, `chrono::Duration`, …)
///   has no single wire shape (the `time` crate emits a `"secs.nanos"` string
///   or a `[secs, nanos]` tuple depending on its features; chrono needs a
///   `serde_with` adapter), so it is documented as any value (`{}`). Annotate
///   the field with `#[schema(...)]` / a newtype to document the exact shape.
fn duration_schema(path: &syn::Path) -> SchemaRef {
    let segments = &path.segments;
    let is_std = segments.len() == 1
        || (segments.len() == 3
            && (segments[0].ident == "std" || segments[0].ident == "core")
            && segments[1].ident == "time");
    if !is_std {
        return SchemaRef::Inline(Box::default());
    }
    let properties = std::collections::BTreeMap::from([
        (
            "secs".to_string(),
            SchemaRef::Inline(Box::new(Schema {
                format: Some("uint64".to_string()),
                minimum: Some(0.0),
                ..Schema::integer()
            })),
        ),
        (
            "nanos".to_string(),
            SchemaRef::Inline(Box::new(Schema {
                format: Some("uint32".to_string()),
                minimum: Some(0.0),
                maximum: Some(999_999_999.0),
                ..Schema::integer()
            })),
        ),
    ]);
    SchemaRef::Inline(Box::new(Schema {
        properties: Some(properties),
        required: Some(vec!["secs".to_string(), "nanos".to_string()]),
        ..Schema::object()
    }))
}

/// Integer schema (with optional OpenAPI format) constrained to `minimum: 1`.
fn non_zero_unsigned(format: Option<&str>) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        format: format.map(str::to_string),
        minimum: Some(1.0),
        ..Schema::integer()
    }))
}

#[cfg(test)]
mod tests;
