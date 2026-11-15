//! Component names of recursive generic instantiations (sidecar module;
//! `use super::*` semantics preserved).

use super::*;
use crate::parser::schema::{
    generic_instances::take_materialized, request_view::with_request_view,
};

fn reference(schema_ref: SchemaRef) -> String {
    match schema_ref {
        SchemaRef::Ref(reference) => reference.ref_path,
        SchemaRef::Inline(schema) => panic!("expected a $ref: {schema:?}"),
    }
}

/// Recursive instantiations over two same-ident types renamed apart
/// (`a::Foo` = `AFoo`, `b::Foo` = `BFoo`) get distinct components.
#[test]
fn recursive_instances_over_scoped_arguments_do_not_collide() {
    take_materialized();
    let known: HashSet<String> = ["Tree", "Foo", "a::Foo", "b::Foo"]
        .into_iter()
        .map(String::from)
        .collect();
    let defs = HashMap::from([
        (
            "Tree".to_string(),
            "struct Tree<T> { value: T, children: Vec<Tree<T>> }".to_string(),
        ),
        (
            "a::Foo".to_string(),
            r#"#[schema(name = "AFoo")] struct Foo { a: u8 }"#.to_string(),
        ),
        (
            "b::Foo".to_string(),
            r#"#[schema(name = "BFoo")] struct Foo { b: u8 }"#.to_string(),
        ),
    ]);
    let reference_of = |src: &str| {
        let ty: Type = syn::parse_str(src).unwrap();
        reference(parse_type_to_schema_ref(&ty, &known, &defs))
    };
    assert_eq!(
        reference_of("Tree<a::Foo>"),
        "#/components/schemas/Tree_AFoo"
    );
    assert_eq!(
        reference_of("Tree<b::Foo>"),
        "#/components/schemas/Tree_BFoo"
    );
    // An unscoped argument keeps its ident.
    assert_eq!(reference_of("Tree<u8>"), "#/components/schemas/Tree_u8");
    let materialized = take_materialized();
    assert_eq!(
        materialized.keys().collect::<Vec<_>>(),
        ["Tree_AFoo", "Tree_BFoo", "Tree_u8"]
    );
}

/// A recursive generic instance whose argument reads differently than it
/// writes (a set: `uniqueItems` on output only) is a separate `.Input`
/// component in the request view.
#[test]
fn recursive_generic_instance_over_a_set_has_an_input_name() {
    take_materialized();
    let known: HashSet<String> = HashSet::from(["Tree".to_string()]);
    let defs = HashMap::from([(
        "Tree".to_string(),
        "struct Tree<T> { v: T, kids: Vec<Tree<T>> }".to_string(),
    )]);
    let ty: Type = syn::parse_str("Tree<HashSet<u8>>").unwrap();
    let response = parse_type_to_schema_ref(&ty, &known, &defs);
    let response_names: Vec<String> = take_materialized().into_keys().collect();
    let request = with_request_view(|| parse_type_to_schema_ref(&ty, &known, &defs));
    let request_names: Vec<String> = take_materialized().into_keys().collect();
    assert_eq!(response_names, ["Tree_HashSet_u8"]);
    assert_eq!(request_names, ["Tree_HashSet_u8.Input"]);
    assert_eq!(reference(response), "#/components/schemas/Tree_HashSet_u8");
    assert_eq!(
        reference(request),
        "#/components/schemas/Tree_HashSet_u8.Input"
    );
}

/// The request view's `Tree<HashSet<Item>>` and the response view's
/// `Tree<HashSet<ItemInput>>` (a real type named `ItemInput`) are different
/// schemas, so they never share a component name.
#[test]
fn request_instance_name_never_collides_with_an_input_named_argument() {
    take_materialized();
    let known: HashSet<String> = ["Tree", "Item", "ItemInput"]
        .into_iter()
        .map(String::from)
        .collect();
    let defs = HashMap::from([
        (
            "Tree".to_string(),
            "struct Tree<T> { v: T, kids: Vec<Tree<T>> }".to_string(),
        ),
        ("Item".to_string(), "struct Item { a: u8 }".to_string()),
        (
            "ItemInput".to_string(),
            "struct ItemInput { b: String }".to_string(),
        ),
    ]);
    let request_ty: Type = syn::parse_str("Tree<HashSet<Item>>").unwrap();
    let response_ty: Type = syn::parse_str("Tree<HashSet<ItemInput>>").unwrap();
    let request = reference(with_request_view(|| {
        parse_type_to_schema_ref(&request_ty, &known, &defs)
    }));
    let response = reference(parse_type_to_schema_ref(&response_ty, &known, &defs));
    assert_ne!(request, response);
    assert_eq!(
        take_materialized().keys().collect::<Vec<_>>(),
        ["Tree_HashSet_Item.Input", "Tree_HashSet_ItemInput"]
    );
}

/// A lifetime-only generic (`Borrowed<'a>`) used as `Borrowed<'static>` is
/// its own (renamed) component, never inlined.
#[test]
fn lifetime_only_definition_is_referenced() {
    let known = HashSet::from(["Borrowed".to_string()]);
    let defs = HashMap::from([(
        "Borrowed".to_string(),
        r#"#[schema(name = "BorrowedName")] struct Borrowed<'a> { name: &'a str }"#.to_string(),
    )]);
    let ty: Type = syn::parse_str("Borrowed<'static>").unwrap();
    assert_eq!(
        reference(parse_type_to_schema_ref(&ty, &known, &defs)),
        "#/components/schemas/BorrowedName"
    );
}
