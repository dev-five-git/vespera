//! Enum to JSON Schema conversion for OpenAPI generation.

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use vespera_core::schema::{Schema, SchemaRef};

use super::serde_attrs::{
    SerdeEnumRepr, extract_deny_unknown_fields, extract_doc_comment, extract_enum_repr,
    extract_rename_all_fields_for, extract_rename_all_for, extract_untagged,
};

mod representations;
mod unit;
mod variant;

pub(super) use representations::{without_other_map, without_single_key_bounds};
pub(super) use unit::derives_serde_repr;

/// The two serde renaming rules an enum container can carry.
#[derive(Clone, Copy)]
struct EnumRenames<'a> {
    /// `rename_all` — applies to variant names.
    variants: Option<&'a str>,
    /// `rename_all_fields` — applies to the fields of struct variants.
    fields: Option<&'a str>,
    /// Container `deny_unknown_fields`: serde's struct-variant visitors read
    /// the ENUM's container attributes, so every struct variant is a closed
    /// object (and an adjacently tagged enum accepts only its tag / content
    /// keys).
    deny_unknown_fields: bool,
}

/// Parses a Rust enum into an OpenAPI Schema.
pub fn parse_enum_to_schema(
    enum_item: &syn::ItemEnum,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    // `List<T>` inside the generic `List<T>` refers back to `List`.
    super::generic_instances::with_definition(
        &enum_item.ident,
        &enum_item.generics,
        &enum_item.attrs,
        || parse_enum_body(enum_item, known_schemas, struct_definitions),
    )
}

fn parse_enum_body(
    enum_item: &syn::ItemEnum,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let enum_description = extract_doc_comment(&enum_item.attrs);
    // `#[schema(value_type)]` / serde `into` / `from` (see `wire_override`).
    if let Some(ty) = super::wire_override::current_container_override(&enum_item.attrs) {
        let schema_ref =
            super::type_schema::parse_type_to_schema_ref(&ty, known_schemas, struct_definitions);
        return super::struct_schema::with_description(schema_ref, enum_description);
    }
    // `serde_repr`: the enum is its integer discriminant on this side.
    if let Some(schema) = unit::parse_serde_repr_enum(enum_item, enum_description.clone()) {
        return schema;
    }
    // No variant takes part on this side (`enum Never {}`, every variant
    // `skip`ped): serde accepts / writes no value at all — `{not: {}}`, never
    // a permissive `{type: string}` or a `discriminator` without `oneOf`.
    if enum_item.variants.iter().all(variant::is_variant_omitted) {
        return Schema {
            description: enum_description,
            not: Some(SchemaRef::Inline(Box::default())),
            ..Schema::default()
        };
    }
    // Names follow the serde direction of the view being built: the request
    // (`{Name}Input`) view uses the deserialize-side renames.
    let side = super::request_view::current_side();
    let rename_all = extract_rename_all_for(&enum_item.attrs, side);
    let rename_all_fields = extract_rename_all_fields_for(&enum_item.attrs, side);
    // Container `rename_all` renames VARIANTS only; struct-variant fields
    // follow `rename_all_fields` (a variant-level `rename_all` still wins).
    let renames = EnumRenames {
        variants: rename_all.as_deref(),
        fields: rename_all_fields.as_deref(),
        deny_unknown_fields: extract_deny_unknown_fields(&enum_item.attrs),
    };
    let repr = extract_enum_repr(&enum_item.attrs);
    if !matches!(repr, SerdeEnumRepr::Untagged)
        && enum_item
            .variants
            .iter()
            .any(|variant| extract_untagged(&variant.attrs))
    {
        return parse_enum_with_untagged_variants(
            enum_item,
            enum_description,
            renames,
            known_schemas,
            struct_definitions,
        );
    }
    let all_unit = enum_item
        .variants
        .iter()
        .filter(|v| !variant::is_variant_omitted(v))
        .all(variant::is_wire_unit);

    if all_unit && matches!(repr, SerdeEnumRepr::ExternallyTagged) {
        return unit::parse_unit_enum_to_schema(enum_item, enum_description, rename_all.as_deref());
    }

    match repr {
        SerdeEnumRepr::ExternallyTagged => representations::parse_externally_tagged_enum(
            enum_item,
            enum_description,
            renames,
            known_schemas,
            struct_definitions,
        ),
        SerdeEnumRepr::InternallyTagged { tag } => representations::parse_internally_tagged_enum(
            enum_item,
            enum_description,
            renames,
            &tag,
            known_schemas,
            struct_definitions,
        ),
        SerdeEnumRepr::AdjacentlyTagged { tag, content } => {
            representations::parse_adjacently_tagged_enum(
                enum_item,
                enum_description,
                renames,
                &tag,
                &content,
                known_schemas,
                struct_definitions,
            )
        }
        SerdeEnumRepr::Untagged => representations::parse_untagged_enum(
            enum_item,
            enum_description,
            renames,
            known_schemas,
            struct_definitions,
        ),
    }
}

/// A tagged enum with per-variant `#[serde(untagged)]` variants.
///
/// serde tries the tagged variants first and falls back to the untagged ones
/// (in declaration order), whose payload is written without any tag. The
/// schema is therefore `anyOf: [<tagged variants>, ...<untagged variants>]` —
/// `anyOf`, because an untagged payload may also match a tagged branch
/// (`Custom(String)` next to the unit variants of a string enum).
fn parse_enum_with_untagged_variants(
    enum_item: &syn::ItemEnum,
    description: Option<String>,
    renames: EnumRenames<'_>,
    known_schemas: &HashSet<impl Borrow<str> + Eq + Hash>,
    struct_definitions: &HashMap<impl Borrow<str> + Eq + Hash, impl AsRef<str>>,
) -> Schema {
    let mut tagged = enum_item.clone();
    let mut untagged = enum_item.clone();
    tagged.attrs.retain(|attr| !attr.path().is_ident("doc"));
    tagged.variants = enum_item
        .variants
        .iter()
        .filter(|variant| !extract_untagged(&variant.attrs))
        .cloned()
        .collect();
    untagged.variants = enum_item
        .variants
        .iter()
        .filter(|variant| extract_untagged(&variant.attrs))
        .cloned()
        .collect();

    let mut any_of = Vec::new();
    if tagged
        .variants
        .iter()
        .any(|variant| !variant::is_variant_omitted(variant))
    {
        any_of.push(SchemaRef::Inline(Box::new(parse_enum_to_schema(
            &tagged,
            known_schemas,
            struct_definitions,
        ))));
    }
    let untagged = representations::parse_untagged_enum(
        &untagged,
        None,
        renames,
        known_schemas,
        struct_definitions,
    );
    any_of.extend(untagged.any_of.unwrap_or_default());
    Schema {
        description,
        any_of: (!any_of.is_empty()).then_some(any_of),
        ..Schema::default()
    }
}

/// Reject enum shapes whose serde representation cannot be described.
///
/// An internally tagged (`#[serde(tag = "..")]`) newtype variant must wrap a
/// map-like value (a struct / map) so serde can inject the tag.  Only inner
/// types that are UNAMBIGUOUSLY non-objects from syntax alone are rejected
/// here (see [`is_unambiguous_non_object`]); a path type such as `Uuid`,
/// `Date` or `Duration` may be a user struct (or, like `std::time::Duration`,
/// serialize as a map), so it is accepted.  Newtypes over a `$ref` to a known
/// non-object component are caught later, once every component is known.
pub fn validate_enum_for_schema(enum_item: &syn::ItemEnum) -> syn::Result<()> {
    let SerdeEnumRepr::InternallyTagged { tag } = extract_enum_repr(&enum_item.attrs) else {
        return Ok(());
    };
    for variant in &enum_item.variants {
        // An untagged variant carries no tag, so any payload is fine.
        if variant::is_variant_omitted(variant) || extract_untagged(&variant.attrs) {
            continue;
        }
        let syn::Fields::Unnamed(fields) = &variant.fields else {
            continue;
        };
        if fields.unnamed.len() != 1 {
            continue;
        }
        let field = &fields.unnamed[0];
        // A custom codec writes / reads a wire shape the type cannot tell.
        let codec = |attrs| {
            let codec = super::serde_attrs::extract_field_codec(attrs);
            codec.serialize || codec.deserialize
        };
        if codec(&variant.attrs) || codec(&field.attrs) {
            continue;
        }
        // `#[schema(value_type = T)]` documents the payload as `T`.
        let value_type = super::schema_attrs::extract_schema_constraints(&field.attrs).value_type;
        let inner = value_type
            .as_ref()
            .map_or(&field.ty, |value_type| &value_type.0);
        if is_unambiguous_non_object(inner) {
            return Err(syn::Error::new_spanned(
                inner,
                internally_tagged_non_object_message(
                    &tag,
                    &variant.ident.to_string(),
                    &quote::quote!(#inner).to_string(),
                ),
            ));
        }
    }
    Ok(())
}

/// Diagnostic for an internally tagged newtype variant whose payload
/// serializes as a non-object value (serde cannot add the tag field to it).
pub fn internally_tagged_non_object_message(tag: &str, variant: &str, inner: &str) -> String {
    format!(
        "internally tagged enum (`#[serde(tag = \"{tag}\")]`): newtype variant `{variant}` \
         must wrap a struct or map so serde can add the `{tag}` field; \
         `{inner}` serializes as a non-object value. Use a struct variant \
         (`{variant} {{ value: ... }}`) or an adjacently tagged enum \
         (`#[serde(tag = \"{tag}\", content = \"...\")]`)"
    )
}

/// `true` only when `ty` serializes as a non-object value no matter what any
/// user type is called: Rust primitives / `str` / `String`, arrays, slices,
/// non-empty tuples, std sequences and sets, `Option<primitive>`, and those
/// behind `&`, `Box`, `Rc` or `Arc`.
fn is_unambiguous_non_object(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Array(_) | syn::Type::Slice(_) => true,
        syn::Type::Tuple(tuple) => !tuple.elems.is_empty(),
        syn::Type::Reference(reference) => is_unambiguous_non_object(&reference.elem),
        syn::Type::Paren(paren) => is_unambiguous_non_object(&paren.elem),
        syn::Type::Group(group) => is_unambiguous_non_object(&group.elem),
        syn::Type::Path(type_path) if type_path.qself.is_none() => {
            let Some((ident, arg)) = std_path_ident(&type_path.path) else {
                return false;
            };
            match ident.as_str() {
                "Vec" | "VecDeque" | "LinkedList" | "BinaryHeap" | "HashSet" | "BTreeSet" => true,
                "Option" => arg.is_some_and(is_primitive_type),
                "Box" | "Rc" | "Arc" => arg.is_some_and(is_unambiguous_non_object),
                _ => is_primitive_ident(&ident),
            }
        }
        _ => false,
    }
}

/// `true` for a primitive (`bool`, integers, floats, `char`, `str`,
/// `String`), possibly behind `&`.
fn is_primitive_type(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(reference) => is_primitive_type(&reference.elem),
        syn::Type::Paren(paren) => is_primitive_type(&paren.elem),
        syn::Type::Group(group) => is_primitive_type(&group.elem),
        syn::Type::Path(type_path) if type_path.qself.is_none() => {
            std_path_ident(&type_path.path).is_some_and(|(ident, _)| is_primitive_ident(&ident))
        }
        _ => false,
    }
}

/// Scalar idents that serialize as a JSON primitive (never an object).
const PRIMITIVE_IDENTS: &[&str] = &[
    "bool", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    "f32", "f64", "char", "str", "String",
];

fn is_primitive_ident(ident: &str) -> bool {
    PRIMITIVE_IDENTS.contains(&ident)
}

/// Last segment ident + its first generic type argument, but only for a bare
/// single-segment path (`Vec<T>`) or one rooted in `std` / `core` / `alloc`
/// (`std::collections::HashSet<T>`) — `my::Vec` is a user type.
fn std_path_ident(path: &syn::Path) -> Option<(String, Option<&syn::Type>)> {
    let rooted_in_std = path.segments.len() == 1
        || path.segments.first().is_some_and(|first| {
            matches!(first.ident.to_string().as_str(), "std" | "core" | "alloc")
        });
    if !rooted_in_std {
        return None;
    }
    let last = path.segments.last()?;
    let arg = match &last.arguments {
        syn::PathArguments::AngleBracketed(args) => args.args.iter().find_map(|arg| match arg {
            syn::GenericArgument::Type(ty) => Some(ty),
            _ => None,
        }),
        _ => None,
    };
    Some((last.ident.to_string(), arg))
}

#[cfg(test)]
mod tests {
    use insta::{assert_debug_snapshot, with_settings};
    use rstest::rstest;

    use super::*;
    use vespera_core::schema::{SchemaRef, SchemaType};

    #[rstest]
    #[case(
        r#"
        #[serde(rename_all = "kebab-case")]
        enum Status {
            #[serde(rename = "ok-status")]
            Ok,
            ErrorCode,
        }
        "#,
        SchemaType::String,
        vec!["ok-status", "error-code"],
        "status"
    )]
    #[case(
        r"
        enum Simple {
            First,
            Second,
        }
        ",
        SchemaType::String,
        vec!["First", "Second"],
        "simple"
    )]
    #[case(
        r#"
        #[serde(rename_all = "snake_case")]
        enum Simple {
            FirstItem,
            SecondItem,
        }
        "#,
        SchemaType::String,
        vec!["first_item", "second_item"],
        "simple_snake"
    )]
    fn test_parse_enum_to_schema_unit_variants(
        #[case] enum_src: &str,
        #[case] expected_type: SchemaType,
        #[case] expected_enum: Vec<&str>,
        #[case] suffix: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        assert_eq!(schema.schema_type, Some(expected_type));
        let got = schema
            .clone()
            .r#enum
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(got, expected_enum);
        with_settings!({ snapshot_path => "snapshots", snapshot_suffix => format!("unit_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r"
        enum Event {
            Data(String),
        }
        ",
        1,
        SchemaType::String,
        0, // single-field tuple variant: the field schema itself
        "tuple_single"
    )]
    #[case(
        r"
        enum Pair {
            Values(i32, String),
        }
        ",
        1,
        SchemaType::Array,
        2, // tuple array prefix_items length
        "tuple_multi"
    )]
    #[case(
        r"
        enum Msg {
            Detail { id: i32, note: Option<String> },
        }
        ",
        1,
        SchemaType::Object,
        0, // not an array: no prefix_items
        "named_object"
    )]
    fn test_parse_enum_to_schema_tuple_and_named_variants(
        #[case] enum_src: &str,
        #[case] expected_one_of_len: usize,
        #[case] expected_inner_type: SchemaType,
        #[case] expected_prefix_items_len: usize,
        #[case] suffix: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.clone().one_of.expect("one_of missing");
        assert_eq!(one_of.len(), expected_one_of_len);

        let SchemaRef::Inline(obj) = &one_of[0] else {
            panic!("Expected inline schema in one_of");
        };
        let props = obj.properties.as_ref().expect("props missing");
        // The variant's payload: the first (only) property value.
        let Some(SchemaRef::Inline(inner)) = props.values().next() else {
            panic!("Expected an inline payload schema");
        };
        // Every case checks the payload type (`tuple_single` too).
        assert_eq!(inner.schema_type, Some(expected_inner_type));
        assert_eq!(
            inner.prefix_items.as_ref().map_or(0, Vec::len),
            expected_prefix_items_len
        );
        if expected_inner_type == SchemaType::Object {
            let inner_props = inner.properties.as_ref().unwrap();
            assert!(inner_props.contains_key("id"));
            assert!(inner_props.contains_key("note"));
            assert!(inner.required.as_ref().unwrap().contains(&"id".to_string()));
        }

        with_settings!({ snapshot_path => "snapshots", snapshot_suffix => format!("tuple_named_{}", suffix) }, {
            assert_debug_snapshot!(schema);
        });
    }

    #[rstest]
    #[case(
        r"
        enum Mixed {
            Ready,
            Data(String),
        }
        ",
        2,
        SchemaType::String,
        "Ready"
    )]
    fn test_parse_enum_to_schema_mixed_unit_variant(
        #[case] enum_src: &str,
        #[case] expected_one_of_len: usize,
        #[case] expected_unit_type: SchemaType,
        #[case] expected_unit_value: &str,
    ) {
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing for mixed enum");
        assert_eq!(one_of.len(), expected_one_of_len);

        let SchemaRef::Inline(unit_schema) = &one_of[0] else {
            panic!("Expected inline schema for unit variant")
        };
        assert_eq!(unit_schema.schema_type, Some(expected_unit_type));
        let unit_enum = unit_schema.r#enum.as_ref().expect("enum values missing");
        assert_eq!(unit_enum[0].as_str().unwrap(), expected_unit_value);
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_for_data_variant() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "kebab-case")]
            enum Payload {
                DataItem(String),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("data-item"));
    }

    #[test]
    fn test_parse_enum_to_schema_field_ignores_enum_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Event {
                Detail { UserId: i32 },
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let SchemaRef::Inline(inner) = props.get("detail").expect("variant key missing") else {
            panic!("Expected inline inner schema")
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");
        // serde: container rename_all renames variants, never their fields.
        assert!(inner_props.contains_key("UserId"));
        assert!(!inner_props.contains_key("user_id"));
    }

    #[test]
    fn test_parse_enum_to_schema_variant_rename_overrides_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Payload {
                #[serde(rename = "Explicit")]
                DataItem(i32),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("Explicit"));
        assert!(!props.contains_key("data_item"));
    }

    #[test]
    fn test_parse_enum_to_schema_field_rename_overrides_variant_rename_all() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "snake_case")]
            enum Payload {
                #[serde(rename_all = "kebab-case")]
                Detail { #[serde(rename = "ID")] user_id: i32 },
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing");
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let SchemaRef::Inline(inner) = props
            .get("detail")
            .or_else(|| props.get("Detail"))
            .expect("variant key missing")
        else {
            panic!("Expected inline inner schema")
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");
        assert!(inner_props.contains_key("ID")); // field-level rename wins
        assert!(!inner_props.contains_key("user-id")); // variant rename_all ignored for this field
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_with_other_attrs_unit() {
        // Test rename_all combined with other serde attributes for unit variants
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "kebab-case", default)]
            enum Status {
                ActiveUser,
                InactiveUser,
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let enum_values = schema.r#enum.expect("enum values missing");
        assert_eq!(enum_values[0].as_str().unwrap(), "active-user");
        assert_eq!(enum_values[1].as_str().unwrap(), "inactive-user");
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_with_other_attrs_data() {
        // Test rename_all combined with other serde attributes for data variants
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            enum Event {
                UserCreated { user_name: String, created_at: i64 },
                UserDeleted(i32),
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing");

        // Check UserCreated variant key is camelCase
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props.contains_key("userCreated"));
        assert!(!props.contains_key("UserCreated"));
        assert!(!props.contains_key("user_created"));

        // Check UserDeleted variant key is camelCase
        let SchemaRef::Inline(variant_obj2) = &one_of[1] else {
            panic!("Expected inline schema")
        };
        let props2 = variant_obj2
            .properties
            .as_ref()
            .expect("variant props missing");
        assert!(props2.contains_key("userDeleted"));
    }

    #[test]
    fn test_parse_enum_to_schema_rename_all_not_first_attr() {
        // Test rename_all when it's not the first attribute
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(default, rename_all = "SCREAMING_SNAKE_CASE")]
            enum Priority {
                HighPriority,
                LowPriority,
            }
        "#,
        )
        .unwrap();

        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let enum_values = schema.r#enum.expect("enum values missing");
        assert_eq!(enum_values[0].as_str().unwrap(), "HIGH_PRIORITY");
        assert_eq!(enum_values[1].as_str().unwrap(), "LOW_PRIORITY");
    }

    // Test enum with empty variants (edge case)
    #[test]
    fn test_parse_enum_to_schema_empty_enum() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r"
            enum Empty {}
        ",
        )
        .unwrap();
        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        // An uninhabited enum accepts no value at all (not any string).
        assert!(schema.r#enum.is_none());
        assert_eq!(schema.schema_type, None);
        assert!(schema.not.is_some());
    }

    // Test enum with all struct variants having empty properties
    #[test]
    fn test_parse_enum_to_schema_struct_variant_no_fields() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r"
            enum Event {
                Empty {},
            }
        ",
        )
        .unwrap();
        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing");
        assert_eq!(one_of.len(), 1);
    }

    // Tests for enum with doc comments on variants
    #[test]
    fn test_parse_enum_to_schema_with_variant_descriptions() {
        let enum_src = r"
            /// Enum description
            enum Status {
                /// Active variant
                Active,
                /// Inactive variant
                Inactive,
            }
        ";
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        assert_eq!(schema.description, Some("Enum description".to_string()));
    }

    #[test]
    fn test_parse_enum_to_schema_data_variant_with_description() {
        let enum_src = r"
            /// Data enum
            enum Event {
                /// Text event description
                Text(String),
                /// Number event description
                Number(i32),
            }
        ";
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        assert_eq!(schema.description, Some("Data enum".to_string()));
        assert!(schema.one_of.is_some());
        let one_of = schema.one_of.unwrap();
        assert_eq!(one_of.len(), 2);
        // Check first variant has description
        if let SchemaRef::Inline(variant_schema) = &one_of[0] {
            assert_eq!(
                variant_schema.description,
                Some("Text event description".to_string())
            );
        }
    }

    #[test]
    fn test_parse_enum_to_schema_struct_variant_with_field_docs() {
        let enum_src = r"
            enum Event {
                /// Record variant
                Record {
                    /// The value field
                    value: i32,
                    /// The name field
                    name: String,
                },
            }
        ";
        let enum_item: syn::ItemEnum = syn::parse_str(enum_src).unwrap();
        let schema = parse_enum_to_schema(
            &enum_item,
            &HashSet::<String>::new(),
            &HashMap::<String, String>::new(),
        );
        assert!(schema.one_of.is_some());
        let one_of = schema.one_of.unwrap();
        if let SchemaRef::Inline(variant_schema) = &one_of[0] {
            assert_eq!(
                variant_schema.description,
                Some("Record variant".to_string())
            );
        }
    }

    #[test]
    fn test_parse_enum_to_schema_variant_field_with_doc_comment_and_ref() {
        // Test that doc comment on field with SchemaRef::Ref wraps in allOf
        let enum_item: syn::ItemEnum = syn::parse_str(
            r"
            enum Message {
                Data {
                    /// The user associated with this message
                    user: User,
                },
            }
        ",
        )
        .unwrap();

        // Register User as a known schema to get SchemaRef::Ref
        let mut known_schemas = HashSet::new();
        known_schemas.insert("User".to_string());

        let schema = parse_enum_to_schema(
            &enum_item,
            &known_schemas,
            &HashMap::<String, String>::new(),
        );
        let one_of = schema.one_of.expect("one_of missing");

        // Get the Data variant schema
        let SchemaRef::Inline(variant_obj) = &one_of[0] else {
            panic!("Expected inline schema")
        };
        let props = variant_obj
            .properties
            .as_ref()
            .expect("variant props missing");
        let SchemaRef::Inline(inner) = props.get("Data").expect("variant key missing") else {
            panic!("Expected inline inner schema")
        };
        let inner_props = inner.properties.as_ref().expect("inner props missing");

        // The user field should have been wrapped in allOf with description
        let user_field = inner_props.get("user").expect("user field missing");
        match user_field {
            SchemaRef::Inline(schema) => {
                // Should have description from doc comment
                assert_eq!(
                    schema.description.as_deref(),
                    Some("The user associated with this message")
                );
                // Should have allOf with the original $ref
                let all_of = schema.all_of.as_ref().expect("allOf missing");
                assert_eq!(all_of.len(), 1);
                let SchemaRef::Ref(reference) = &all_of[0] else {
                    panic!("Expected $ref in allOf")
                };
                assert_eq!(reference.ref_path, "#/components/schemas/User");
            }
            SchemaRef::Ref(_) => panic!("Expected inline schema with allOf, not direct $ref"),
        }
    }
}

#[cfg(test)]
mod tests_validate;
