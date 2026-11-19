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
