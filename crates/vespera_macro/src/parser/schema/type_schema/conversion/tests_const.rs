//! Const generic arguments of an inlined instantiation (sidecar module;
//! `use super::*` semantics preserved): `Buf<u8, 4>` pins `[T; N]` to four
//! elements.

use rstest::rstest;

use super::*;
use crate::parser::schema::generic_instances::take_materialized;

fn inline_schema(src: &str, known: &HashSet<String>, defs: &HashMap<String, String>) -> Schema {
    let ty: Type = syn::parse_str(src).unwrap();
    match parse_type_to_schema_ref(&ty, known, defs) {
        SchemaRef::Inline(schema) => *schema,
        SchemaRef::Ref(reference) => panic!("expected an inline schema: {reference:?}"),
    }
}

/// Round 28: a const argument reaches the array length of the inlined field,
/// so the field is a fixed-length array (`minItems` = `maxItems`), exactly as
/// serde reads it.
#[rstest]
#[case::literal_argument("Buf<u8, 4>", Some(4))]
fn const_arguments_pin_inlined_array_lengths(#[case] src: &str, #[case] len: Option<usize>) {
    let known: HashSet<String> = ["Buf"].into_iter().map(String::from).collect();
    let defs = HashMap::from([(
        "Buf".to_string(),
        "struct Buf<T, const N: usize> { data: [T; N] }".to_string(),
    )]);
    let schema = inline_schema(src, &known, &defs);
    let properties = schema.properties.expect("the struct's fields");
    let SchemaRef::Inline(data) = &properties["data"] else {
        panic!("the array is inline");
    };
    assert_eq!(data.min_items, len);
    assert_eq!(data.max_items, len);
}

fn reference(schema_ref: SchemaRef) -> String {
    match schema_ref {
        SchemaRef::Ref(reference) => reference.ref_path,
        SchemaRef::Inline(schema) => panic!("expected a $ref: {schema:?}"),
    }
}

/// Round 28: a recursive instantiation is a named component, and its name
/// carries the const arguments: `Buf<u8, 4>` and `Buf<u8, 8>` (and the
/// `Buf<T, N>` instance each one reaches inside itself) never share one.
#[test]
fn recursive_const_instances_get_distinct_components() {
    take_materialized();
    let known: HashSet<String> = ["Buf"].into_iter().map(String::from).collect();
    let defs = HashMap::from([(
        "Buf".to_string(),
        "struct Buf<T, const N: usize> { data: [T; N], children: Vec<Buf<T, N>> }".to_string(),
    )]);
    let reference_of = |src: &str| {
        let ty: Type = syn::parse_str(src).unwrap();
        reference(parse_type_to_schema_ref(&ty, &known, &defs))
    };
    let four = reference_of("Buf<u8, 4>");
    let eight = reference_of("Buf<u8, 8>");
    assert_ne!(four, eight);
    let materialized = take_materialized();
    let keys: Vec<&String> = materialized.keys().collect();
    assert!(keys.iter().any(|key| key.contains("_Const.4")), "{keys:?}");
    assert!(keys.iter().all(|key| !key.contains("_Const.N")), "{keys:?}");
}

/// Round 30: a const argument of a NESTED type argument is part of the name
/// too. `Tree<Buf<u8, 4>>` and `Tree<Buf<u8, 8>>` are distinct recursive
/// components, and each one's `value` keeps its own array length (before the
/// fix both were named `Tree_Buf_u8`, so the second reused the first).
#[test]
fn nested_const_arguments_get_distinct_recursive_components() {
    take_materialized();
    let known: HashSet<String> = ["Tree", "Buf"].into_iter().map(String::from).collect();
    let defs = HashMap::from([
        (
            "Buf".to_string(),
            "struct Buf<T, const N: usize> { data: [T; N] }".to_string(),
        ),
        (
            "Tree".to_string(),
            "struct Tree<T> { value: T, children: Vec<Tree<T>> }".to_string(),
        ),
    ]);
    let reference_of = |src: &str| {
        let ty: Type = syn::parse_str(src).unwrap();
        reference(parse_type_to_schema_ref(&ty, &known, &defs))
    };
    let four = reference_of("Tree<Buf<u8, 4>>");
    let eight = reference_of("Tree<Buf<u8, 8>>");
    assert_ne!(four, eight);
    let materialized = take_materialized();
    for (path, len) in [(four, 4), (eight, 8)] {
        let name = path.trim_start_matches("#/components/schemas/");
        let component = &materialized[name];
        let properties = component.properties.as_ref().expect("the tree's fields");
        let SchemaRef::Inline(value) = &properties["value"] else {
            panic!("the buffer is inline");
        };
        let value_properties = value.properties.as_ref().expect("the buffer's fields");
        let SchemaRef::Inline(data) = &value_properties["data"] else {
            panic!("the array is inline");
        };
        assert_eq!(data.min_items, Some(len), "{name}");
        assert_eq!(data.max_items, Some(len), "{name}");
    }
}

/// Round 30: a definition with only const parameters (`Matrix<const N>`) has
/// no type to substitute, but its `Matrix<3>` argument is concrete: it inlines
/// with the length pinned, as a definition with a type parameter does.
#[test]
fn const_only_definition_inlines_with_its_length() {
    let known: HashSet<String> = ["Matrix"].into_iter().map(String::from).collect();
    let defs = HashMap::from([(
        "Matrix".to_string(),
        "struct Matrix<const N: usize> { data: [f32; N] }".to_string(),
    )]);
    let schema = inline_schema("Matrix<3>", &known, &defs);
    let properties = schema.properties.expect("the struct's fields");
    let SchemaRef::Inline(data) = &properties["data"] else {
        panic!("the array is inline");
    };
    assert_eq!(data.min_items, Some(3));
    assert_eq!(data.max_items, Some(3));
}

/// Round 30: a const parameter named in an attribute type (`value_type =
/// [T; N]`) is pinned by the instantiation like one named in a field type, so
/// the documented array has the argument's fixed length.
#[test]
fn const_arguments_pin_lengths_in_attribute_types() {
    let known: HashSet<String> = ["Buf"].into_iter().map(String::from).collect();
    let defs = HashMap::from([(
        "Buf".to_string(),
        "struct Buf<T, const N: usize> { #[schema(value_type = [T; N])] data: Vec<T> }".to_string(),
    )]);
    let schema = inline_schema("Buf<u8, 4>", &known, &defs);
    let properties = schema.properties.expect("the struct's fields");
    let SchemaRef::Inline(data) = &properties["data"] else {
        panic!("the array is inline");
    };
    assert_eq!(data.min_items, Some(4));
    assert_eq!(data.max_items, Some(4));
}

/// Round 29: a const argument written as a bare identifier (`Buf<u8, LEN>`,
/// parsed as a type) pairs with its const parameter, so the type parameter
/// still takes `u8` and the vector's items are integers, as with `Buf<u8, 4>`.
#[rstest]
#[case::bare_identifier("Buf<u8, LEN>")]
#[case::literal("Buf<u8, 4>")]
fn bare_identifier_const_argument_keeps_type_substitution(#[case] src: &str) {
    let known: HashSet<String> = ["Buf"].into_iter().map(String::from).collect();
    let defs = HashMap::from([(
        "Buf".to_string(),
        "struct Buf<T, const N: usize> { items: Vec<T>, data: [T; N] }".to_string(),
    )]);
    let schema = inline_schema(src, &known, &defs);
    let properties = schema.properties.expect("the struct's fields");
    let SchemaRef::Inline(items) = &properties["items"] else {
        panic!("the vector is inline");
    };
    let Some(SchemaRef::Inline(item)) = &items.items else {
        panic!("the vector's items are inline");
    };
    assert_eq!(item.schema_type, Some(SchemaType::Integer));
}

/// Round 29: a const parameter left out takes its declared default
/// (`const N: usize = 4`), as a type parameter left out takes its default.
#[test]
fn missing_const_argument_takes_the_declared_default() {
    let known: HashSet<String> = ["Buf"].into_iter().map(String::from).collect();
    let defs = HashMap::from([(
        "Buf".to_string(),
        "struct Buf<T, const N: usize = 4> { data: [T; N] }".to_string(),
    )]);
    let schema = inline_schema("Buf<u8>", &known, &defs);
    let properties = schema.properties.expect("the struct's fields");
    let SchemaRef::Inline(data) = &properties["data"] else {
        panic!("the array is inline");
    };
    assert_eq!(data.min_items, Some(4));
    assert_eq!(data.max_items, Some(4));
}

/// Round 29: const substitution runs before the type arguments are placed, so
/// a concrete type argument spelled like a const parameter (`Buf<N, 4>` with a
/// user type `N`) stays that type: `Vec<N>` is a `$ref` to it, and `[N; 4]`
/// still pins four elements.
#[test]
fn const_substitution_keeps_a_type_argument_named_like_a_const() {
    let known: HashSet<String> = ["Buf", "N"].into_iter().map(String::from).collect();
    let defs = HashMap::from([
        (
            "Buf".to_string(),
            "struct Buf<T, const N: usize> { items: Vec<T>, data: [T; N] }".to_string(),
        ),
        ("N".to_string(), "struct N { x: u8 }".to_string()),
    ]);
    let schema = inline_schema("Buf<N, 4>", &known, &defs);
    let properties = schema.properties.expect("the struct's fields");
    let SchemaRef::Inline(items) = &properties["items"] else {
        panic!("the vector is inline");
    };
    assert_eq!(
        reference(items.items.clone().expect("the vector's items")),
        Reference::schema("N").ref_path
    );
    let SchemaRef::Inline(data) = &properties["data"] else {
        panic!("the array is inline");
    };
    assert_eq!(data.min_items, Some(4));
    assert_eq!(
        reference(data.items.clone().expect("the array's items")),
        Reference::schema("N").ref_path
    );
}
