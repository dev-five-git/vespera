use std::collections::{HashMap, HashSet};

use syn::Type;
use vespera_core::schema::SchemaRef;

use crate::{
    parser::schema::{
        SerdeSide, definition_key, extract_field_rename_for, extract_rename_all_for,
        extract_transparent, is_primitive_type, nullable_schema_ref, rename_field,
        single_emitted_field, strip_raw_prefix_owned, substitute_type,
        wire_override::container_override_type,
    },
    schema_macro::type_utils::is_primitive_like as utils_is_primitive_like,
};

pub(super) fn is_primitive_or_like(ty: &Type) -> bool {
    is_primitive_type(ty) || utils_is_primitive_like(ty)
}

pub(super) fn convert_to_inline_schema(field_schema: SchemaRef, is_optional: bool) -> SchemaRef {
    if is_optional {
        return nullable_schema_ref(field_schema);
    }
    field_schema
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
        // A segment-less path names no known type: report `false` rather than panic.
        let Some(segment) = type_path.path.segments.last() else {
            return false;
        };
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

/// The struct named by `ty`'s last path segment, when `struct_definitions`
/// holds it as a struct with **named** fields (see [`struct_definition`]).
pub(super) fn named_struct_fields(
    ty: &Type,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<syn::ItemStruct> {
    struct_definition(ty, struct_definitions)
        .filter(|struct_item| matches!(struct_item.fields, syn::Fields::Named(_)))
}

/// The struct named by `ty`'s last path segment, of any shape (named, tuple
/// or unit), when `struct_definitions` holds it.
///
/// A generic struct's field types are instantiated with `ty`'s type
/// arguments (`Paged<Filter>` → `T` = `Filter`; a missing argument takes the
/// parameter's default), so a flattened `T` or a `T`-typed field documents
/// the concrete type.
pub(super) fn struct_definition(
    ty: &Type,
    struct_definitions: &HashMap<&str, &str>,
) -> Option<syn::ItemStruct> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    let key = definition_key(&type_path.path, |key| struct_definitions.contains_key(key))?;
    let mut struct_item =
        syn::parse_str::<syn::ItemStruct>(struct_definitions.get(key.as_str())?).ok()?;
    let names: Vec<String> = struct_item
        .generics
        .type_params()
        .map(|param| param.ident.to_string())
        .collect();
    if names.is_empty() {
        return Some(struct_item);
    }
    let args: Vec<&Type> = match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|arg| match arg {
                syn::GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    // Each parameter: its argument, else its default (which may name earlier
    // parameters, already concrete), else itself (left unresolved).
    let mut concrete: Vec<Type> = Vec::with_capacity(names.len());
    for (index, param) in struct_item.generics.type_params().enumerate() {
        let ty = args.get(index).map_or_else(
            || {
                param.default.as_ref().map_or_else(
                    || {
                        let ident = &param.ident;
                        syn::parse_quote!(#ident)
                    },
                    |(_, default)| {
                        let earlier: Vec<&Type> = concrete.iter().collect();
                        substitute_type(default, &names[..index], &earlier)
                    },
                )
            },
            |arg| (*arg).clone(),
        );
        concrete.push(ty);
    }
    let concrete: Vec<&Type> = concrete.iter().collect();
    for field in &mut struct_item.fields {
        field.ty = substitute_type(&field.ty, &names, &concrete);
    }
    Some(struct_item)
}

/// How many container `from` / `try_from` / `transparent` hops are followed
/// (a cycle stops).
pub(super) const MAX_CONVERSION_DEPTH: usize = 8;

/// The type serde reads INSTEAD of `struct_item`'s own fields (named or
/// tuple): the container `from` / `try_from` (or `#[schema(value_type)]`)
/// source, or the transparent field of a `#[serde(transparent)]` struct —
/// serde's rule: the only field that is not `PhantomData`, not skipped and
/// not `#[serde(default)]` on the deserialize side.
pub(super) fn deserialized_instead(struct_item: &syn::ItemStruct) -> Option<Type> {
    container_override_type(&struct_item.attrs, SerdeSide::Deserialize).or_else(|| {
        extract_transparent(&struct_item.attrs)
            .then(|| single_emitted_field(struct_item.fields.iter(), SerdeSide::Deserialize))
            .flatten()
            .map(|field| field.ty.clone())
    })
}

/// `ty` with every container conversion / `transparent` hop followed: the
/// type a `Query<ty>` / `Path<ty>` actually reads (`ty` itself when it is
/// not such a struct).
pub(super) fn deserialized_source(ty: &Type, struct_definitions: &HashMap<&str, &str>) -> Type {
    let mut current = ty.clone();
    for _ in 0..MAX_CONVERSION_DEPTH {
        match struct_definition(&current, struct_definitions)
            .as_ref()
            .and_then(deserialized_instead)
        {
            Some(source) => current = source,
            None => break,
        }
    }
    current
}

/// Whether `struct_definitions` holds `ty`'s last path segment as a struct of
/// any shape (named, tuple or unit).
pub(super) fn is_defined_struct(ty: &Type, struct_definitions: &HashMap<&str, &str>) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    definition_key(&type_path.path, |key| struct_definitions.contains_key(key))
        .and_then(|key| struct_definitions.get(key.as_str()))
        .is_some_and(|definition| syn::parse_str::<syn::ItemStruct>(definition).is_ok())
}

/// The container `rename_all` rule serde applies when DESERIALIZING — path
/// and query parameters are input only.
pub(super) fn input_rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    extract_rename_all_for(attrs, SerdeSide::Deserialize)
}

/// The wire name serde reads a named field from (the deserialize side, since
/// parameters are input only): `#[serde(rename)]` wins over the container's
/// `rename_all`; a raw identifier loses its `r#` prefix.
pub(super) fn serde_field_name(field: &syn::Field, rename_all: Option<&str>) -> String {
    extract_field_rename_for(&field.attrs, SerdeSide::Deserialize).unwrap_or_else(|| {
        let rust_name = field.ident.as_ref().map_or_else(String::new, |ident| {
            strip_raw_prefix_owned(ident.to_string())
        });
        rename_field(&rust_name, rename_all)
    })
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
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: Punctuated::new(),
            },
        });
        assert!(!is_known_type(&ty, &HashSet::new(), &HashMap::new()));
    }

    #[test]
    fn known_type_non_path_type_is_false() {
        let ty: Type = syn::parse_str("(i32, String)").unwrap();
        assert!(!is_known_type(&ty, &HashSet::new(), &HashMap::new()));
        assert!(!is_defined_struct(&ty, &HashMap::new()));
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
