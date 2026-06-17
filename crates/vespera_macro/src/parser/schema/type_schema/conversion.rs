//! Type to JSON Schema conversion for `OpenAPI` generation.
//!
//! This module handles the conversion of Rust types (as parsed by syn)
//! into OpenAPI-compatible JSON Schema references and inline schemas.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
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

/// Check if a type is a primitive Rust type that maps directly to a JSON Schema type.
/// Inline integer schema with an OpenAPI format string.
fn integer_with_format(format: &str) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        format: Some(format.to_string()),
        ..Schema::integer()
    }))
}

/// Inline number schema with an OpenAPI format string.
fn number_with_format(format: &str) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        format: Some(format.to_string()),
        ..Schema::number()
    }))
}

/// Inline string schema with an OpenAPI format string.
fn string_with_format(format: &str) -> SchemaRef {
    SchemaRef::Inline(Box::new(Schema {
        format: Some(format.to_string()),
        ..Schema::string()
    }))
}

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

/// Converts a Rust type to an `OpenAPI` `SchemaRef`.
///
/// This is the main entry point for type-to-schema conversion.
pub fn parse_type_to_schema_ref(
    ty: &Type,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> SchemaRef {
    parse_type_to_schema_ref_with_schemas(ty, known_schemas, struct_definitions)
}

/// Type-to-schema conversion with depth-guarded recursion.
///
/// Handles:
/// - Primitive types (i32, String, bool, etc.)
/// - Generic wrappers (Vec, Option, Box)
/// - `SeaORM` relations (`HasOne`, `HasMany`)
/// - Map types (`HashMap`, `BTreeMap`)
/// - Date/time types (`DateTime`, `NaiveDate`, etc.)
/// - Known schema references
/// - Generic type instantiation
pub fn parse_type_to_schema_ref_with_schemas(
    ty: &Type,
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> SchemaRef {
    SCHEMA_RECURSION_DEPTH.with(|depth| {
        let current = depth.get();
        if current >= MAX_SCHEMA_RECURSION_DEPTH {
            return SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)));
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
    known_schemas: &HashSet<String>,
    struct_definitions: &HashMap<String, String>,
) -> SchemaRef {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            if path.segments.is_empty() {
                return SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)));
            }

            // Get the last segment as the type name (handles paths like crate::TestStruct)
            let segment = path.segments.last().unwrap();
            let ident_str = segment.ident.to_string();

            // Handle generic types
            if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                match ident_str.as_str() {
                    // Box<T> -> T's schema (Box is just heap allocation, transparent for schema)
                    "Box" => {
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                            return parse_type_to_schema_ref(
                                inner_ty,
                                known_schemas,
                                struct_definitions,
                            );
                        }
                    }
                    "Vec" | "HashSet" | "BTreeSet" | "Option" => {
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                            let inner_schema = parse_type_to_schema_ref(
                                inner_ty,
                                known_schemas,
                                struct_definitions,
                            );
                            if ident_str == "Vec" {
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
            match ident_str.as_str() {
                // Signed integers: use OpenAPI format registry
                // https://spec.openapis.org/registry/format/index.html
                "i8" => integer_with_format("int8"),
                "i16" => integer_with_format("int16"),
                "i32" => integer_with_format("int32"),
                "i64" => integer_with_format("int64"),
                // Unsigned integers: use OpenAPI format registry
                "u8" => integer_with_format("uint8"),
                "u16" => integer_with_format("uint16"),
                "u32" => integer_with_format("uint32"),
                "u64" => integer_with_format("uint64"),
                // i128, isize, StatusCode: no standard format in the registry
                "i128" | "isize" | "StatusCode" => SchemaRef::Inline(Box::new(Schema::integer())),
                // u128, usize: unsigned with no standard format — use minimum: 0
                "u128" | "usize" => SchemaRef::Inline(Box::new(Schema {
                    minimum: Some(0.0),
                    ..Schema::integer()
                })),
                "f32" => number_with_format("float"),
                "f64" => number_with_format("double"),
                // `rust_decimal` serializes `Decimal` as a JSON *string* (to
                // preserve precision), so the wire type is string, not number.
                "Decimal" => string_with_format("decimal"),
                "bool" => SchemaRef::Inline(Box::new(Schema::boolean())),
                "char" => string_with_format("char"),
                "Uuid" => string_with_format("uuid"),
                "String" | "str" => SchemaRef::Inline(Box::new(Schema::string())),
                // Date-time types from chrono and time crates
                "DateTime"
                | "NaiveDateTime"
                | "DateTimeWithTimeZone"
                | "DateTimeUtc"
                | "DateTimeLocal"
                | "OffsetDateTime"
                | "PrimitiveDateTime" => string_with_format("date-time"),
                "NaiveDate" | "Date" => string_with_format("date"),
                "NaiveTime" | "Time" => string_with_format("time"),
                // Duration types
                "Duration" => string_with_format("duration"),
                // File upload types (vespera::multipart / tempfile)
                // FieldData<NamedTempFile> → string with binary format
                "FieldData" | "NamedTempFile" => string_with_format("binary"),
                // Standard library types that should not be referenced
                // Note: HashMap and BTreeMap are handled above in generic types
                "Vec" | "HashSet" | "BTreeSet" | "Option" | "Result" | "Json" | "Path"
                | "Query" | "Header" => {
                    // These are not schema types, return object schema
                    SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                }
                _ => {
                    // Check if this is a known schema (struct with Schema derive)
                    // Use just the type name (handles both crate::TestStruct and TestStruct)
                    let type_name = ident_str.clone();

                    // For paths like `module::Schema`, try to find the schema name
                    // by checking if there's a schema named `ModuleSchema` or `ModuleNameSchema`
                    let resolved_name = if type_name == "Schema" && path.segments.len() > 1 {
                        // Get the parent module name (e.g., "user" from "crate::models::user::Schema")
                        let parent_segment = &path.segments[path.segments.len() - 2];
                        let parent_name = parent_segment.ident.to_string();

                        // Try PascalCase version: "user" -> "UserSchema"
                        // Rust identifiers are guaranteed non-empty
                        let pascal_name = format!("{}Schema", capitalize_first(&parent_name));

                        if known_schemas.contains(&pascal_name) {
                            pascal_name
                        } else {
                            // Try lowercase version: "userSchema"
                            let lower_name = format!("{parent_name}Schema");
                            if known_schemas.contains(&lower_name) {
                                lower_name
                            } else {
                                type_name
                            }
                        }
                    } else {
                        type_name
                    };

                    if known_schemas.contains(&resolved_name) {
                        if let Some(def) = struct_definitions.get(&resolved_name)
                            && let Ok(parsed_struct) = syn::parse_str::<syn::ItemStruct>(def)
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
                            if let Some(base_def) = struct_definitions.get(&resolved_name)
                                && let Ok(mut parsed) = syn::parse_str::<syn::ItemStruct>(base_def)
                            {
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
                                    if let syn::Fields::Named(fields_named) = &mut parsed.fields {
                                        for field in &mut fields_named.named {
                                            field.ty = substitute_type(
                                                &field.ty,
                                                &generic_params,
                                                &concrete_types,
                                            );
                                        }
                                    }

                                    // Remove generics from the struct (it's now concrete)
                                    parsed.generics.params.clear();
                                    parsed.generics.where_clause = None;

                                    // Parse the substituted struct to schema (inline)
                                    let schema = parse_struct_to_schema(
                                        &parsed,
                                        known_schemas,
                                        struct_definitions,
                                    );
                                    return SchemaRef::Inline(Box::new(schema));
                                }
                            }
                        }
                        // Non-generic type or generic without parameters - use reference
                        SchemaRef::Ref(Reference::schema(&resolved_name))
                    } else {
                        // For unknown custom types, return object schema instead of reference
                        // This prevents creating invalid references to non-existent schemas
                        SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object)))
                    }
                }
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
        _ => SchemaRef::Inline(Box::new(Schema::new(SchemaType::Object))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // ========== Coverage: generic known schema edge cases ==========

    #[test]
    fn test_generic_known_schema_no_struct_definition() {
        // Known schema with angle brackets but NO struct_definitions entry → falls through to Ref
        let mut known = HashSet::new();
        known.insert("Wrapper".to_string());
        // Do NOT insert into struct_definitions
        let ty: Type = syn::parse_str("Wrapper<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());
        // Should fall through to non-generic ref path
        assert!(
            matches!(schema_ref, SchemaRef::Ref(_)),
            "Should be a $ref when no struct definition found"
        );
    }

    #[test]
    fn test_generic_known_schema_param_count_mismatch() {
        // Struct has 1 generic param but 2 concrete types provided → falls through to Ref
        let mut known = HashSet::new();
        known.insert("Single".to_string());
        let mut defs = HashMap::new();
        defs.insert(
            "Single".to_string(),
            "struct Single<T> { value: T }".to_string(),
        );

        let ty: Type = syn::parse_str("Single<String, i32>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
        assert!(
            matches!(schema_ref, SchemaRef::Ref(_)),
            "Mismatched param count should fall through to $ref"
        );
    }

    #[test]
    fn test_generic_known_schema_invalid_definition() {
        // struct_definitions has invalid Rust code → parse fails → falls through to Ref
        let mut known = HashSet::new();
        known.insert("Bad".to_string());
        let mut defs = HashMap::new();
        defs.insert("Bad".to_string(), "not valid rust code!!!".to_string());

        let ty: Type = syn::parse_str("Bad<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
        assert!(
            matches!(schema_ref, SchemaRef::Ref(_)),
            "Invalid definition should fall through to $ref"
        );
    }

    #[test]
    fn test_generic_known_schema_tuple_struct() {
        // Tuple struct fields are NOT Named → skips field substitution but still inlines
        let mut known = HashSet::new();
        known.insert("Pair".to_string());
        let mut defs = HashMap::new();
        defs.insert("Pair".to_string(), "struct Pair<T>(T, T);".to_string());

        let ty: Type = syn::parse_str("Pair<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
        // Tuple struct still gets inlined (generics cleared, parse_struct_to_schema called)
        // but field types are NOT substituted (no Named fields to iterate)
        assert!(
            matches!(schema_ref, SchemaRef::Inline(_)),
            "Tuple struct should still inline"
        );
    }

    #[test]
    fn test_generic_known_schema_no_generic_params_in_def() {
        // Struct definition has no generics but concrete type has angle brackets → mismatch
        let mut known = HashSet::new();
        known.insert("Plain".to_string());
        let mut defs = HashMap::new();
        defs.insert("Plain".to_string(), "struct Plain { x: i32 }".to_string());

        let ty: Type = syn::parse_str("Plain<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
        // 0 generic params != 1 concrete type → falls through to Ref
        assert!(matches!(schema_ref, SchemaRef::Ref(_)));
    }

    // ========== Coverage: nested generic types ==========

    #[test]
    fn test_nested_vec_vec_string() {
        let ty: Type = syn::parse_str("Vec<Vec<String>>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            if let Some(SchemaRef::Inline(inner)) = schema.items.as_deref() {
                assert_eq!(inner.schema_type, Some(SchemaType::Array));
                if let Some(SchemaRef::Inline(innermost)) = inner.items.as_deref() {
                    assert_eq!(innermost.schema_type, Some(SchemaType::String));
                } else {
                    panic!("Expected innermost inline schema");
                }
            } else {
                panic!("Expected inner inline schema");
            }
        } else {
            panic!("Expected inline schema for nested Vec");
        }
    }

    #[test]
    fn test_option_vec_i32() {
        let ty: Type = syn::parse_str("Option<Vec<i32>>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            assert_eq!(schema.nullable, Some(true));
            if let Some(SchemaRef::Inline(items)) = schema.items.as_deref() {
                assert_eq!(items.schema_type, Some(SchemaType::Integer));
            } else {
                panic!("Expected inline items");
            }
        } else {
            panic!("Expected inline schema for Option<Vec<i32>>");
        }
    }

    #[test]
    fn test_box_box_i32() {
        // Box<Box<i32>> → transparent twice → integer
        let ty: Type = syn::parse_str("Box<Box<i32>>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline integer schema for Box<Box<i32>>");
        }
    }

    // ========== Coverage: HashMap/BTreeMap with known ref value ==========

    #[test]
    fn test_hashmap_with_known_ref_value() {
        let mut known = HashSet::new();
        known.insert("User".to_string());
        let ty: Type = syn::parse_str("HashMap<String, User>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Object));
            let additional = schema.additional_properties.as_ref().unwrap();
            assert_eq!(additional.get("$ref").unwrap(), "#/components/schemas/User");
        } else {
            panic!("Expected inline schema for HashMap<String, User>");
        }
    }

    #[test]
    fn test_btreemap_with_inline_value() {
        let ty: Type = syn::parse_str("BTreeMap<String, Vec<i32>>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Object));
            let additional = schema.additional_properties.as_ref().unwrap();
            // Value should be an array schema serialized
            assert_eq!(additional.get("type").unwrap(), "array");
        } else {
            panic!("Expected inline schema for BTreeMap with Vec value");
        }
    }

    // ========== Coverage: HashMap/BTreeMap with insufficient args ==========

    #[test]
    fn test_hashmap_single_arg_falls_through() {
        // HashMap<String> — only 1 type arg, need 2 → falls through to unknown type
        let ty: Type = syn::parse_str("HashMap<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Object));
            // Should NOT have additional_properties since it fell through
            assert!(schema.additional_properties.is_none());
        } else {
            panic!("Expected inline schema");
        }
    }

    // ========== Coverage: &mut T reference ==========

    #[test]
    fn test_mutable_reference_delegates_to_inner() {
        let ty: Type = syn::parse_str("&mut String").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
        } else {
            panic!("Expected inline string schema for &mut String");
        }
    }

    // ========== Coverage: HashSet/BTreeSet → uniqueItems ==========

    #[test]
    fn test_hashset_string_produces_unique_items_array() {
        let ty: Type = syn::parse_str("HashSet<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            assert_eq!(schema.unique_items, Some(true));
            if let Some(SchemaRef::Inline(items)) = schema.items.as_deref() {
                assert_eq!(items.schema_type, Some(SchemaType::String));
            } else {
                panic!("Expected inline string items for HashSet<String>");
            }
        } else {
            panic!("Expected inline schema for HashSet<String>");
        }
    }

    #[test]
    fn test_btreeset_i32_produces_unique_items_array() {
        let ty: Type = syn::parse_str("BTreeSet<i32>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            assert_eq!(schema.unique_items, Some(true));
            if let Some(SchemaRef::Inline(items)) = schema.items.as_deref() {
                assert_eq!(items.schema_type, Some(SchemaType::Integer));
            } else {
                panic!("Expected inline integer items for BTreeSet<i32>");
            }
        } else {
            panic!("Expected inline schema for BTreeSet<i32>");
        }
    }

    #[test]
    fn test_option_hashset_is_nullable_unique_array() {
        let ty: Type = syn::parse_str("Option<HashSet<i64>>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            assert_eq!(schema.unique_items, Some(true));
            assert_eq!(schema.nullable, Some(true));
            if let Some(SchemaRef::Inline(items)) = schema.items.as_deref() {
                assert_eq!(items.schema_type, Some(SchemaType::Integer));
            } else {
                panic!("Expected inline integer items for Option<HashSet<i64>>");
            }
        } else {
            panic!("Expected inline schema for Option<HashSet<i64>>");
        }
    }

    #[test]
    fn test_vec_does_not_have_unique_items() {
        let ty: Type = syn::parse_str("Vec<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = &schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            assert!(schema.unique_items.is_none());
        } else {
            panic!("Expected inline schema for Vec<String>");
        }
    }

    #[test]
    fn test_bare_hashset_without_generics() {
        // HashSet without angle brackets → falls through to bare-name match
        let ty: Type = syn::parse_str("HashSet").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Inline(_)));
    }

    #[test]
    fn test_bare_btreeset_without_generics() {
        let ty: Type = syn::parse_str("BTreeSet").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Inline(_)));
    }

    #[test]
    fn test_known_schema_ref_override_returns_inline_ref_schema() {
        let mut known = HashSet::new();
        known.insert("UserSchema".to_string());

        let mut defs = HashMap::new();
        defs.insert(
            "UserSchema".to_string(),
            r#"
            #[schema(ref = "ExternalUser", nullable)]
            struct UserSchema {
                id: i32,
            }
            "#
            .to_string(),
        );

        let ty: Type = syn::parse_str("UserSchema").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);

        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(
                    schema.ref_path.as_deref(),
                    Some("#/components/schemas/ExternalUser")
                );
                assert_eq!(schema.nullable, Some(true));
            }
            SchemaRef::Ref(_) => panic!("expected inline schema ref override"),
        }
    }
}
