use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::schema::{Schema, SchemaRef};

use crate::{
    parser::schema::is_primitive_type,
    schema_macro::type_utils::is_primitive_like as utils_is_primitive_like,
};

pub(super) fn is_primitive_or_like(ty: &Type) -> bool {
    is_primitive_type(ty) || utils_is_primitive_like(ty)
}

pub(super) fn convert_to_inline_schema(field_schema: SchemaRef, is_optional: bool) -> SchemaRef {
    match field_schema {
        SchemaRef::Inline(mut schema) => {
            if is_optional {
                schema.nullable = Some(true);
            }
            SchemaRef::Inline(schema)
        }
        SchemaRef::Ref(r) if is_optional => SchemaRef::Inline(Box::new(Schema {
            ref_path: Some(r.ref_path),
            schema_type: None,
            nullable: Some(true),
            ..Default::default()
        })),
        SchemaRef::Ref(r) => SchemaRef::Ref(r),
    }
}

pub(super) fn is_known_type(
    ty: &Type,
    known_schemas: &HashSet<&str>,
    struct_definitions: &HashMap<&str, &str>,
) -> bool {
    if is_primitive_type(ty) {
        return true;
    }

    if let Type::Path(type_path) = ty {
        let path = &type_path.path;
        if path.segments.is_empty() {
            return false;
        }

        let segment = path.segments.last().unwrap();
        let ident_str = segment.ident.to_string();
        if struct_definitions.contains_key(ident_str.as_str())
            || known_schemas.contains(ident_str.as_str())
        {
            return true;
        }

        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
            match ident_str.as_str() {
                "Vec" | "HashSet" | "BTreeSet" | "Option" => {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        return is_known_type(inner_ty, known_schemas, struct_definitions);
                    }
                }
                _ => {}
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use rstest::rstest;
    use syn::Type;
    use vespera_core::schema::{Reference, Schema, SchemaRef, SchemaType};

    use super::*;
    use crate::schema_macro::type_utils::is_map_type as utils_is_map_type;

    #[rstest]
    #[case("String", true)]
    #[case("i32", true)]
    #[case("Vec<String>", true)]
    #[case("Option<bool>", true)]
    #[case("CustomType", false)]
    fn primitive_like(#[case] type_str: &str, #[case] expected: bool) {
        let ty: Type = syn::parse_str(type_str).unwrap();
        assert_eq!(is_primitive_or_like(&ty), expected);
    }

    #[rstest]
    #[case("HashMap<String, String>", true)]
    #[case("BTreeMap<String, String>", true)]
    #[case("String", false)]
    #[case("Vec<i32>", false)]
    fn map_type(#[case] type_str: &str, #[case] expected: bool) {
        let ty: Type = syn::parse_str(type_str).unwrap();
        assert_eq!(utils_is_map_type(&ty), expected);
    }

    #[rstest]
    #[case("i32", HashSet::new(), HashMap::new(), true)]
    #[case("User", HashSet::new(), {
        let mut map = HashMap::new();
        map.insert("User", "pub struct User { id: i32 }");
        map
    }, true)]
    #[case("Product", {
        let mut set = HashSet::new();
        set.insert("Product");
        set
    }, HashMap::new(), true)]
    #[case("Vec<i32>", HashSet::new(), HashMap::new(), true)]
    #[case("Option<String>", HashSet::new(), HashMap::new(), true)]
    #[case("UnknownType", HashSet::new(), HashMap::new(), false)]
    fn known_type(
        #[case] type_str: &str,
        #[case] known_schemas: HashSet<&str>,
        #[case] struct_definitions: HashMap<&str, &str>,
        #[case] expected: bool,
    ) {
        let ty: Type = syn::parse_str(type_str).unwrap();
        assert_eq!(
            is_known_type(&ty, &known_schemas, &struct_definitions),
            expected
        );
    }

    #[test]
    fn known_type_empty_segments() {
        use syn::punctuated::Punctuated;

        let ty = Type::Path(syn::TypePath {
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: Punctuated::new(),
            },
        });
        assert!(!is_known_type(&ty, &HashSet::new(), &HashMap::new()));
    }

    #[test]
    fn known_type_non_vec_option_generic() {
        let known_schemas = HashSet::new();
        let struct_definitions = HashMap::new();
        let ty: Type = syn::parse_str("Box<i32>").unwrap();
        assert!(!is_known_type(&ty, &known_schemas, &struct_definitions));
        let ty: Type = syn::parse_str("Result<i32, String>").unwrap();
        assert!(!is_known_type(&ty, &known_schemas, &struct_definitions));
    }

    #[test]
    fn convert_to_inline_schema_inline() {
        let schema = SchemaRef::Inline(Box::new(Schema::string()));
        let result = convert_to_inline_schema(schema, false);
        let SchemaRef::Inline(s) = result else {
            panic!("Expected Inline")
        };
        assert_eq!(s.schema_type, Some(SchemaType::String));
        assert!(s.nullable.is_none());
    }

    #[test]
    fn convert_to_inline_schema_inline_optional() {
        let schema = SchemaRef::Inline(Box::new(Schema::string()));
        let result = convert_to_inline_schema(schema, true);
        let SchemaRef::Inline(s) = result else {
            panic!("Expected Inline")
        };
        assert_eq!(s.schema_type, Some(SchemaType::String));
        assert_eq!(s.nullable, Some(true));
    }

    #[test]
    fn convert_to_inline_schema_ref_optional_preserves_ref_path() {
        let schema = SchemaRef::Ref(Reference {
            ref_path: "#/components/schemas/User".to_string(),
        });
        let result = convert_to_inline_schema(schema, true);
        let SchemaRef::Inline(s) = result else {
            panic!("Expected Inline wrapper")
        };
        assert_eq!(s.ref_path, Some("#/components/schemas/User".to_string()));
        assert_eq!(s.nullable, Some(true));
        assert_eq!(s.schema_type, None);
    }

    #[test]
    fn convert_to_inline_schema_ref_required_passes_through() {
        let schema = SchemaRef::Ref(Reference::schema("SomeType"));
        let result = convert_to_inline_schema(schema, false);
        let SchemaRef::Ref(r) = result else {
            panic!("Expected $ref")
        };
        assert_eq!(r.ref_path, "#/components/schemas/SomeType");
    }

    #[test]
    fn convert_to_inline_schema_ref_optional_wraps_nullable() {
        let schema = SchemaRef::Ref(Reference::schema("User"));
        let result = convert_to_inline_schema(schema, true);
        let SchemaRef::Inline(s) = result else {
            panic!("Expected Inline wrapper")
        };
        assert_eq!(s.ref_path, Some("#/components/schemas/User".to_string()));
        assert_eq!(s.nullable, Some(true));
        assert_eq!(s.schema_type, None);
    }
}
