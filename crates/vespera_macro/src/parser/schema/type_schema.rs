//! Type to JSON Schema conversion for `OpenAPI` generation.
//!
//! This module handles the conversion of Rust types (as parsed by syn)
//! into OpenAPI-compatible JSON Schema references and inline schemas.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
};

use syn::Type;
use vespera_core::schema::{Reference, Schema, SchemaRef, SchemaType};

/// Maximum recursion depth for type-to-schema conversion.
/// Prevents stack overflow from deeply nested or circular type references.
const MAX_SCHEMA_RECURSION_DEPTH: usize = 32;

thread_local! {
    static SCHEMA_RECURSION_DEPTH: Cell<usize> = const { Cell::new(0) };
}

use super::{
    generics::substitute_type,
    serde_attrs::{capitalize_first, extract_schema_name_from_entity},
    struct_schema::parse_struct_to_schema,
};

/// Check if a type is a primitive Rust type that maps directly to a JSON Schema type.
pub fn is_primitive_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => {
            let path = &type_path.path;
            if path.segments.len() == 1 {
                let ident = path.segments[0].ident.to_string();
                matches!(
                    ident.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "i128"
                        | "isize"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "String"
                        | "str"
                        | "Decimal"
                )
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
                    "Vec" | "Option" => {
                        if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                            let inner_schema = parse_type_to_schema_ref(
                                inner_ty,
                                known_schemas,
                                struct_definitions,
                            );
                            if ident_str == "Vec" {
                                return SchemaRef::Inline(Box::new(Schema::array(inner_schema)));
                            }
                            // Option<T> -> nullable schema
                            match inner_schema {
                                SchemaRef::Inline(mut schema) => {
                                    schema.nullable = Some(true);
                                    return SchemaRef::Inline(schema);
                                }
                                SchemaRef::Ref(reference) => {
                                    // Wrap reference in an inline schema to attach nullable flag
                                    return SchemaRef::Inline(Box::new(Schema {
                                        ref_path: Some(reference.ref_path),
                                        schema_type: None,
                                        nullable: Some(true),
                                        ..Schema::new(SchemaType::Object)
                                    }));
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
                            return SchemaRef::Inline(Box::new(Schema {
                                ref_path: Some(format!("#/components/schemas/{schema_name}")),
                                schema_type: None,
                                nullable: Some(true),
                                ..Schema::new(SchemaType::Object)
                            }));
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
                            // Convert SchemaRef to serde_json::Value for additional_properties
                            let additional_props_value = match value_schema {
                                SchemaRef::Ref(ref_ref) => {
                                    serde_json::json!({ "$ref": ref_ref.ref_path })
                                }
                                SchemaRef::Inline(schema) => serde_json::to_value(&*schema)
                                    .unwrap_or_else(|_| serde_json::json!({})),
                            };
                            return SchemaRef::Inline(Box::new(Schema {
                                schema_type: Some(SchemaType::Object),
                                additional_properties: Some(additional_props_value),
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
                "i8" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("int8".to_string()),
                    ..Schema::integer()
                })),
                "i16" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("int16".to_string()),
                    ..Schema::integer()
                })),
                "i32" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("int32".to_string()),
                    ..Schema::integer()
                })),
                "i64" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("int64".to_string()),
                    ..Schema::integer()
                })),
                // Unsigned integers: use OpenAPI format registry
                "u8" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("uint8".to_string()),
                    ..Schema::integer()
                })),
                "u16" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("uint16".to_string()),
                    ..Schema::integer()
                })),
                "u32" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("uint32".to_string()),
                    ..Schema::integer()
                })),
                "u64" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("uint64".to_string()),
                    ..Schema::integer()
                })),
                // i128, isize, StatusCode: no standard format in the registry
                "i128" | "isize" | "StatusCode" => SchemaRef::Inline(Box::new(Schema::integer())),
                // u128, usize: unsigned with no standard format — use minimum: 0
                "u128" | "usize" => SchemaRef::Inline(Box::new(Schema {
                    minimum: Some(0.0),
                    ..Schema::integer()
                })),
                "f32" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("float".to_string()),
                    ..Schema::number()
                })),
                "f64" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("double".to_string()),
                    ..Schema::number()
                })),
                "Decimal" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("decimal".to_string()),
                    ..Schema::number()
                })),
                "bool" => SchemaRef::Inline(Box::new(Schema::boolean())),
                "char" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("char".to_string()),
                    ..Schema::string()
                })),
                "Uuid" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("uuid".to_string()),
                    ..Schema::string()
                })),
                "String" | "str" => SchemaRef::Inline(Box::new(Schema::string())),
                // Date-time types from chrono and time crates
                "DateTime"
                | "NaiveDateTime"
                | "DateTimeWithTimeZone"
                | "DateTimeUtc"
                | "DateTimeLocal"
                | "OffsetDateTime"
                | "PrimitiveDateTime" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("date-time".to_string()),
                    ..Schema::string()
                })),
                "NaiveDate" | "Date" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("date".to_string()),
                    ..Schema::string()
                })),
                "NaiveTime" | "Time" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("time".to_string()),
                    ..Schema::string()
                })),
                // Duration types
                "Duration" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("duration".to_string()),
                    ..Schema::string()
                })),
                // File upload types (axum_typed_multipart / tempfile)
                // FieldData<NamedTempFile> → string with binary format
                "FieldData" | "NamedTempFile" => SchemaRef::Inline(Box::new(Schema {
                    format: Some("binary".to_string()),
                    ..Schema::string()
                })),
                // Standard library types that should not be referenced
                // Note: HashMap and BTreeMap are handled above in generic types
                "Vec" | "Option" | "Result" | "Json" | "Path" | "Query" | "Header" => {
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
    use rstest::rstest;
    use vespera_core::schema::SchemaType;

    use super::*;

    #[rstest]
    #[case("HashMap<String, i32>", Some(SchemaType::Object), true)]
    #[case("Option<String>", Some(SchemaType::String), false)] // nullable check
    fn test_parse_type_to_schema_ref_cases(
        #[case] ty_src: &str,
        #[case] expected_type: Option<SchemaType>,
        #[case] expect_additional_props: bool,
    ) {
        let ty: syn::Type = syn::parse_str(ty_src).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, expected_type);
            if expect_additional_props {
                assert!(schema.additional_properties.is_some());
            }
            if ty_src.starts_with("Option") {
                assert_eq!(schema.nullable, Some(true));
            }
        } else {
            panic!("Expected inline schema for {ty_src}");
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_option_ref_nullable() {
        let mut known = HashSet::new();
        known.insert("User".to_string());

        let ty: syn::Type = syn::parse_str("Option<User>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());

        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(
                    schema.ref_path,
                    Some("#/components/schemas/User".to_string())
                );
                assert_eq!(schema.nullable, Some(true));
                assert_eq!(schema.schema_type, None);
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema for Option<User>"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_empty_path_and_reference() {
        // Empty path segments returns object
        let ty = Type::Path(syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Inline(_)));

        // Reference type delegates to inner
        let ty: Type = syn::parse_str("&i32").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline integer schema");
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_known_schema_ref_and_unknown_custom() {
        let mut known_schemas = HashSet::new();
        known_schemas.insert("Known".to_string());

        let ty: Type = syn::parse_str("Known").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Ref(_)));

        let ty: Type = syn::parse_str("UnknownType").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &HashMap::new());
        assert!(matches!(schema_ref, SchemaRef::Inline(_)));
    }

    #[test]
    fn test_parse_type_to_schema_ref_generic_substitution() {
        // Ensure generic struct Wrapper<T> { value: T } is substituted to concrete type
        let mut known_schemas = HashSet::new();
        known_schemas.insert("Wrapper".to_string());

        let mut struct_definitions = HashMap::new();
        struct_definitions.insert(
            "Wrapper".to_string(),
            "struct Wrapper<T> { value: T }".to_string(),
        );

        let ty: syn::Type = syn::parse_str("Wrapper<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &struct_definitions);

        if let SchemaRef::Inline(schema) = schema_ref {
            let props = schema.properties.as_ref().unwrap();
            let value = props.get("value").unwrap();
            if let SchemaRef::Inline(inner) = value {
                assert_eq!(inner.schema_type, Some(SchemaType::String));
            } else {
                panic!("Expected inline schema for value");
            }
        } else {
            panic!("Expected inline schema for generic substitution");
        }
    }

    #[rstest]
    #[case("&i32")]
    #[case("std::string::String")]
    fn test_is_primitive_type_non_path_variants(#[case] ty_src: &str) {
        let ty: Type = syn::parse_str(ty_src).unwrap();
        assert!(!is_primitive_type(&ty));
    }

    #[rstest]
    #[case(
        "HashMap<String, Value>",
        true,
        None,
        Some("#/components/schemas/Value")
    )]
    #[case("Result<String, i32>", false, Some(SchemaType::Object), None)]
    #[case("crate::Value", false, None, None)]
    #[case("(i32, bool)", false, Some(SchemaType::Object), None)]
    fn test_parse_type_to_schema_ref_additional_cases(
        #[case] ty_src: &str,
        #[case] expect_additional_props: bool,
        #[case] expected_type: Option<SchemaType>,
        #[case] expected_ref: Option<&str>,
    ) {
        let mut known_schemas = HashSet::new();
        known_schemas.insert("Value".to_string());

        let ty: Type = syn::parse_str(ty_src).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &HashMap::new());
        match expected_ref {
            Some(expected) => {
                let SchemaRef::Inline(schema) = schema_ref else {
                    panic!("Expected inline schema for {ty_src}");
                };
                let additional = schema
                    .additional_properties
                    .as_ref()
                    .expect("additional_properties missing");
                assert_eq!(additional.get("$ref").unwrap(), expected);
            }
            None => match schema_ref {
                SchemaRef::Inline(schema) => {
                    if expect_additional_props {
                        assert!(schema.additional_properties.is_some());
                    } else {
                        assert_eq!(schema.schema_type, expected_type);
                    }
                }
                SchemaRef::Ref(_) => {
                    assert!(ty_src.contains("Value"));
                }
            },
        }
    }

    // Test Vec without inner type (edge case)
    #[test]
    fn test_parse_type_to_schema_ref_vec_without_args() {
        let ty: Type = syn::parse_str("Vec").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        // Vec without angle brackets should return object schema
        assert!(matches!(schema_ref, SchemaRef::Inline(_)));
    }

    // Test parse_type_to_schema_ref with unknown custom type (not in known_schemas)
    #[test]
    fn test_parse_type_to_schema_ref_unknown_custom_type() {
        // MyUnknownType is not in known_schemas, should return inline object schema
        let ty: Type = syn::parse_str("MyUnknownType").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Object));
        } else {
            panic!("Expected inline schema for unknown type");
        }
    }

    // Test parse_type_to_schema_ref with qualified path to unknown type
    #[test]
    fn test_parse_type_to_schema_ref_qualified_unknown_type() {
        // crate::models::UnknownStruct is not in known_schemas
        let ty: Type = syn::parse_str("crate::models::UnknownStruct").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Object));
        } else {
            panic!("Expected inline schema for unknown qualified type");
        }
    }

    // Test BTreeMap type
    #[test]
    fn test_parse_type_to_schema_ref_btreemap() {
        let ty: Type = syn::parse_str("BTreeMap<String, i32>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Object));
            assert!(schema.additional_properties.is_some());
        } else {
            panic!("Expected inline schema for BTreeMap");
        }
    }

    // Coverage tests for Box<T> type handling
    #[test]
    fn test_parse_type_to_schema_ref_box_type() {
        let ty: Type = syn::parse_str("Box<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        // Box<T> should be transparent - returns T's schema
        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(schema.schema_type, Some(SchemaType::String));
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema for Box<String>"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_box_with_known_type() {
        let mut known = HashSet::new();
        known.insert("User".to_string());
        let ty: Type = syn::parse_str("Box<User>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());
        // Box<User> should return User's schema ref
        match schema_ref {
            SchemaRef::Ref(reference) => {
                assert_eq!(reference.ref_path, "#/components/schemas/User");
            }
            SchemaRef::Inline(_) => panic!("Expected ref for Box<User>"),
        }
    }

    // Coverage tests for HasOne<Entity> handling
    #[test]
    fn test_parse_type_to_schema_ref_has_one_entity() {
        // HasOne<super::user::Entity> should produce nullable ref to UserSchema
        let ty: Type = syn::parse_str("HasOne<super::user::Entity>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        match schema_ref {
            SchemaRef::Inline(schema) => {
                // Should have ref_path to UserSchema and be nullable
                assert_eq!(
                    schema.ref_path,
                    Some("#/components/schemas/User".to_string())
                );
                assert_eq!(schema.nullable, Some(true));
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema for HasOne"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_has_one_fallback() {
        // HasOne<i32> should fallback to generic object (no Entity)
        let ty: Type = syn::parse_str("HasOne<i32>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        match schema_ref {
            SchemaRef::Inline(schema) => {
                // Fallback: generic object
                assert_eq!(schema.schema_type, Some(SchemaType::Object));
                assert!(schema.ref_path.is_none());
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema for HasOne fallback"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_has_one_non_entity_path() {
        // HasOne<crate::models::User> - path doesn't end with Entity
        let ty: Type = syn::parse_str("HasOne<crate::models::User>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        match schema_ref {
            SchemaRef::Inline(schema) => {
                // Fallback: generic object since not "Entity"
                assert_eq!(schema.schema_type, Some(SchemaType::Object));
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema"),
        }
    }

    // Coverage tests for HasMany<Entity> handling
    #[test]
    fn test_parse_type_to_schema_ref_has_many_entity() {
        // HasMany<super::comment::Entity> should produce array of refs
        let ty: Type = syn::parse_str("HasMany<super::comment::Entity>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        match schema_ref {
            SchemaRef::Inline(schema) => {
                // Should be array type
                assert_eq!(schema.schema_type, Some(SchemaType::Array));
                // Items should be ref to CommentSchema
                if let Some(SchemaRef::Ref(items_ref)) = schema.items.as_deref() {
                    assert_eq!(items_ref.ref_path, "#/components/schemas/Comment");
                } else {
                    panic!("Expected items to be a $ref");
                }
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema for HasMany"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_has_many_fallback() {
        // HasMany<String> should fallback to array of objects
        let ty: Type = syn::parse_str("HasMany<String>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        match schema_ref {
            SchemaRef::Inline(schema) => {
                assert_eq!(schema.schema_type, Some(SchemaType::Array));
                // Items should be inline object
                if let Some(SchemaRef::Inline(items)) = schema.items.as_deref() {
                    assert_eq!(items.schema_type, Some(SchemaType::Object));
                } else {
                    panic!("Expected inline items for HasMany fallback");
                }
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema for HasMany fallback"),
        }
    }

    // Coverage tests for Schema path resolution
    #[test]
    fn test_parse_type_to_schema_ref_module_schema_path_pascal() {
        // crate::models::user::Schema should resolve to UserSchema if in known_schemas
        let mut known = HashSet::new();
        known.insert("UserSchema".to_string());
        let ty: Type = syn::parse_str("crate::models::user::Schema").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());
        match schema_ref {
            SchemaRef::Ref(reference) => {
                assert_eq!(reference.ref_path, "#/components/schemas/UserSchema");
            }
            SchemaRef::Inline(_) => panic!("Expected $ref for module::Schema"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_module_schema_path_lower() {
        // crate::models::user::Schema should resolve to userSchema if PascalCase not found
        let mut known = HashSet::new();
        known.insert("userSchema".to_string());
        let ty: Type = syn::parse_str("crate::models::user::Schema").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());
        match schema_ref {
            SchemaRef::Ref(reference) => {
                assert_eq!(reference.ref_path, "#/components/schemas/userSchema");
            }
            SchemaRef::Inline(_) => panic!("Expected $ref for module::Schema with lowercase"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_module_schema_path_fallback() {
        // crate::models::user::Schema with no known schemas should use Schema as-is
        let ty: Type = syn::parse_str("crate::models::user::Schema").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        // Falls through to unknown type handling
        match schema_ref {
            SchemaRef::Inline(schema) => {
                // Unknown custom type defaults to object
                assert_eq!(schema.schema_type, Some(SchemaType::Object));
            }
            SchemaRef::Ref(_) => panic!("Expected inline for unknown Schema type"),
        }
    }

    #[test]
    fn test_parse_type_to_schema_ref_module_schema_with_empty_parent() {
        // Test the branch for module::Schema where PascalCase conversion handles edge case
        // Use a path like `::Schema` which has empty segments before Schema
        let ty: Type = syn::parse_str("Schema").unwrap();

        // Register schemas to trigger the module::Schema lookup path
        let mut known = HashSet::new();
        known.insert("Schema".to_string());

        let schema_ref = parse_type_to_schema_ref(&ty, &known, &HashMap::new());
        match schema_ref {
            SchemaRef::Ref(reference) => {
                assert_eq!(reference.ref_path, "#/components/schemas/Schema");
            }
            SchemaRef::Inline(_) => panic!("Expected $ref for Schema type"),
        }
    }

    // Tests for date/time types from chrono crate
    #[rstest]
    #[case("DateTime", "date-time")]
    #[case("NaiveDateTime", "date-time")]
    #[case("DateTimeWithTimeZone", "date-time")]
    #[case("DateTimeUtc", "date-time")]
    #[case("DateTimeLocal", "date-time")]
    #[case("NaiveDate", "date")]
    #[case("NaiveTime", "time")]
    fn test_parse_type_to_schema_ref_chrono_date_time_types(
        #[case] ty_name: &str,
        #[case] expected_format: &str,
    ) {
        let ty: Type = syn::parse_str(ty_name).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());

        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(
                schema.schema_type,
                Some(SchemaType::String),
                "Type {ty_name} should be string schema"
            );
            assert_eq!(
                schema.format,
                Some(expected_format.to_string()),
                "Type {ty_name} should have format {expected_format}"
            );
        } else {
            panic!("Expected inline schema for {ty_name}");
        }
    }

    // Tests for date/time types from time crate
    #[rstest]
    #[case("OffsetDateTime", "date-time")]
    #[case("PrimitiveDateTime", "date-time")]
    #[case("Date", "date")]
    #[case("Time", "time")]
    fn test_parse_type_to_schema_ref_time_crate_types(
        #[case] ty_name: &str,
        #[case] expected_format: &str,
    ) {
        let ty: Type = syn::parse_str(ty_name).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());

        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(
                schema.schema_type,
                Some(SchemaType::String),
                "Type {ty_name} should be string schema"
            );
            assert_eq!(
                schema.format,
                Some(expected_format.to_string()),
                "Type {ty_name} should have format {expected_format}"
            );
        } else {
            panic!("Expected inline schema for {ty_name}");
        }
    }

    // Test for Duration type
    #[test]
    fn test_parse_type_to_schema_ref_duration() {
        let ty: Type = syn::parse_str("Duration").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());

        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
            assert_eq!(schema.format, Some("duration".to_string()));
        } else {
            panic!("Expected inline schema for Duration");
        }
    }

    // Test for qualified chrono types (e.g., chrono::DateTime<Utc>)
    #[test]
    fn test_parse_type_to_schema_ref_qualified_chrono_types() {
        // Test with module-qualified paths
        let qualified_types = vec![
            ("chrono::DateTime<chrono::Utc>", "date-time"),
            ("chrono::NaiveDate", "date"),
            ("chrono::NaiveTime", "time"),
        ];

        for (ty_str, expected_format) in qualified_types {
            let ty: Type = syn::parse_str(ty_str).unwrap();
            let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());

            if let SchemaRef::Inline(schema) = schema_ref {
                assert_eq!(
                    schema.schema_type,
                    Some(SchemaType::String),
                    "Type {ty_str} should be string schema"
                );
                assert_eq!(
                    schema.format,
                    Some(expected_format.to_string()),
                    "Type {ty_str} should have format {expected_format}"
                );
            } else {
                panic!("Expected inline schema for {ty_str}");
            }
        }
    }

    // Test for Option<date/time type> (ensures nullable is preserved)
    #[rstest]
    #[case("Option<DateTime>", "date-time")]
    #[case("Option<NaiveDate>", "date")]
    #[case("Option<Duration>", "duration")]
    fn test_parse_type_to_schema_ref_optional_date_time_types(
        #[case] ty_str: &str,
        #[case] expected_format: &str,
    ) {
        let ty: Type = syn::parse_str(ty_str).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());

        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
            assert_eq!(schema.format, Some(expected_format.to_string()));
            assert_eq!(schema.nullable, Some(true), "{ty_str} should be nullable");
        } else {
            panic!("Expected inline schema for {ty_str}");
        }
    }

    // Test for Vec<date/time type> (array of date/time values)
    #[test]
    fn test_parse_type_to_schema_ref_vec_date_time_types() {
        let ty: Type = syn::parse_str("Vec<DateTime>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());

        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Array));
            if let Some(SchemaRef::Inline(items)) = schema.items.as_deref() {
                assert_eq!(items.schema_type, Some(SchemaType::String));
                assert_eq!(items.format, Some("date-time".to_string()));
            } else {
                panic!("Expected inline items schema");
            }
        } else {
            panic!("Expected inline schema for Vec<DateTime>");
        }
    }

    // Test for Box<date/time type> (should be transparent)
    #[test]
    fn test_parse_type_to_schema_ref_box_date_time_types() {
        let ty: Type = syn::parse_str("Box<NaiveDate>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());

        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
            assert_eq!(schema.format, Some("date".to_string()));
        } else {
            panic!("Expected inline schema for Box<NaiveDate>");
        }
    }

    // Test generic struct with date/time type parameter (lines 289, 302)
    #[test]
    fn test_parse_type_to_schema_ref_generic_with_date_time_parameter() {
        let mut known_schemas = HashSet::new();
        known_schemas.insert("Event".to_string());

        let mut struct_definitions = HashMap::new();
        // Generic struct with a date/time type parameter
        struct_definitions.insert(
            "Event".to_string(),
            "struct Event<T> { timestamp: T, name: String }".to_string(),
        );

        // Concrete instantiation with DateTime
        let ty: Type = syn::parse_str("Event<DateTime>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &struct_definitions);

        if let SchemaRef::Inline(schema) = schema_ref {
            let props = schema.properties.as_ref().unwrap();

            // Check timestamp field is DateTime with correct format
            let timestamp_schema = props.get("timestamp").unwrap();
            if let SchemaRef::Inline(ts) = timestamp_schema {
                assert_eq!(ts.schema_type, Some(SchemaType::String));
                assert_eq!(ts.format, Some("date-time".to_string()));
            } else {
                panic!("Expected inline schema for timestamp field");
            }

            // Check name field is String
            let name_schema = props.get("name").unwrap();
            if let SchemaRef::Inline(n) = name_schema {
                assert_eq!(n.schema_type, Some(SchemaType::String));
            } else {
                panic!("Expected inline schema for name field");
            }
        } else {
            panic!("Expected inline schema for generic Event<DateTime>");
        }
    }

    // Test multiple generic parameters with date/time types
    #[test]
    fn test_parse_type_to_schema_ref_generic_multiple_date_time_params() {
        let mut known_schemas = HashSet::new();
        known_schemas.insert("TimeRange".to_string());

        let mut struct_definitions = HashMap::new();
        struct_definitions.insert(
            "TimeRange".to_string(),
            "struct TimeRange<T, U> { start: T, end: U }".to_string(),
        );

        let ty: Type = syn::parse_str("TimeRange<DateTime, NaiveDate>").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &known_schemas, &struct_definitions);

        if let SchemaRef::Inline(schema) = schema_ref {
            let props = schema.properties.as_ref().unwrap();

            // Check start field is DateTime
            let start = props.get("start").unwrap();
            if let SchemaRef::Inline(s) = start {
                assert_eq!(s.format, Some("date-time".to_string()));
            } else {
                panic!("Expected inline for start");
            }

            // Check end field is NaiveDate
            let end = props.get("end").unwrap();
            if let SchemaRef::Inline(e) = end {
                assert_eq!(e.format, Some("date".to_string()));
            } else {
                panic!("Expected inline for end");
            }
        } else {
            panic!("Expected inline schema for TimeRange");
        }
    }

    #[rstest]
    #[case("i8")]
    #[case("i16")]
    #[case("i32")]
    #[case("i64")]
    #[case("u8")]
    #[case("u16")]
    #[case("u32")]
    #[case("u64")]
    #[case("f32")]
    #[case("f64")]
    #[case("bool")]
    #[case("String")]
    #[case("str")]
    fn test_is_primitive_type_positive(#[case] ty_src: &str) {
        let ty: Type = syn::parse_str(ty_src).unwrap();
        assert!(is_primitive_type(&ty), "{ty_src} should be primitive");
    }

    #[rstest]
    #[case("Vec")]
    #[case("Option")]
    #[case("HashMap")]
    #[case("MyStruct")]
    fn test_is_primitive_type_negative_single_segment(#[case] ty_src: &str) {
        let ty: Type = syn::parse_str(ty_src).unwrap();
        assert!(!is_primitive_type(&ty), "{ty_src} should NOT be primitive");
    }

    #[test]
    fn test_is_primitive_type_tuple_type() {
        let ty: Type = syn::parse_str("(i32, bool)").unwrap();
        assert!(!is_primitive_type(&ty));
    }

    // ========== Coverage: FieldData / NamedTempFile binary format ==========

    #[test]
    fn test_parse_type_field_data_binary_format() {
        let ty: Type = syn::parse_str("FieldData").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
            assert_eq!(schema.format, Some("binary".to_string()));
        } else {
            panic!("Expected inline schema for FieldData");
        }
    }

    #[test]
    fn test_parse_type_named_temp_file_binary_format() {
        let ty: Type = syn::parse_str("NamedTempFile").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::String));
            assert_eq!(schema.format, Some("binary".to_string()));
        } else {
            panic!("Expected inline schema for NamedTempFile");
        }
    }

    // ========== Coverage: StatusCode → integer ==========

    #[test]
    fn test_parse_type_status_code_integer() {
        let ty: Type = syn::parse_str("StatusCode").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline schema for StatusCode");
        }
    }

    #[test]
    fn test_parse_type_qualified_status_code_integer() {
        // axum::http::StatusCode should also map to integer (last segment matching)
        let ty: Type = syn::parse_str("axum::http::StatusCode").unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(schema.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline schema for axum::http::StatusCode");
        }
    }

    // ========== Coverage: non-generic wrapper types without angle brackets ==========

    #[rstest]
    #[case("Option")]
    #[case("Result")]
    #[case("Json")]
    #[case("Path")]
    #[case("Query")]
    #[case("Header")]
    fn test_parse_type_non_generic_wrappers_return_object(#[case] ty_src: &str) {
        let ty: Type = syn::parse_str(ty_src).unwrap();
        let schema_ref = parse_type_to_schema_ref(&ty, &HashSet::new(), &HashMap::new());
        if let SchemaRef::Inline(schema) = schema_ref {
            assert_eq!(
                schema.schema_type,
                Some(SchemaType::Object),
                "{ty_src} without generics should be object"
            );
        } else {
            panic!("Expected inline schema for {ty_src}");
        }
    }

    // ========== Coverage: recursion depth limit ==========

    #[test]
    fn test_recursion_depth_limit_returns_object() {
        SCHEMA_RECURSION_DEPTH.with(|depth| {
            let previous = depth.get();
            depth.set(MAX_SCHEMA_RECURSION_DEPTH);
            let ty: Type = syn::parse_str("String").unwrap();
            let schema_ref =
                parse_type_to_schema_ref_with_schemas(&ty, &HashSet::new(), &HashMap::new());
            // Should return object fallback, NOT string
            if let SchemaRef::Inline(schema) = &schema_ref {
                assert_eq!(schema.schema_type, Some(SchemaType::Object));
            } else {
                panic!("Expected inline object schema at max recursion depth");
            }
            // Restore
            depth.set(previous);
        });
    }

    #[test]
    fn test_recursion_depth_resets_after_call() {
        SCHEMA_RECURSION_DEPTH.with(|depth| {
            assert_eq!(depth.get(), 0, "Depth should start at 0");
        });
        let ty: Type = syn::parse_str("Vec<Option<String>>").unwrap();
        let _ = parse_type_to_schema_ref_with_schemas(&ty, &HashSet::new(), &HashMap::new());
        SCHEMA_RECURSION_DEPTH.with(|depth| {
            assert_eq!(depth.get(), 0, "Depth should reset to 0 after call");
        });
    }

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
}
