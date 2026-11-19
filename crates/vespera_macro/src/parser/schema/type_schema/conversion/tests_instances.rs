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

/// The generic definition `Tree<T, M = Meta>` built as its own component:
/// its self-reference `Tree<T>` (`Tree<T, Meta>`) is the component `Tree`, not
/// a second component `Tree_T_Meta` with an unresolved `T`.
#[test]
fn self_reference_omitting_a_defaulted_parameter_is_the_component() {
    take_materialized();
    let src = "struct Tree<T, M = Meta> { value: T, kids: Vec<Tree<T>> }";
    let item: syn::ItemStruct = syn::parse_str(src).unwrap();
    let known = HashSet::from(["Tree", "Meta"]);
    let defs = HashMap::from([("Tree", src), ("Meta", "struct Meta { x: u8 }")]);
    let schema = crate::parser::schema::parse_struct_to_schema(&item, &known, &defs);
    let kids = schema
        .properties
        .as_ref()
        .and_then(|props| props.get("kids"))
        .expect("kids property");
    let SchemaRef::Inline(kids) = kids else {
        panic!("kids is an inline array: {kids:?}");
    };
    let items = kids.items.clone().expect("array items");
    assert_eq!(reference(items), "#/components/schemas/Tree");
    assert!(
        take_materialized().is_empty(),
        "no second component for the defaulted instance"
    );
}

/// Polymorphic recursion (`Nested<T>` holds `Nested<Vec<T>>` twice) never
/// repeats an instance, so no `$ref` cuts it, and its expansion doubles with
/// every level. Its expansion is capped per top-level instance: the instances
/// past the cap are documented as any value (`{}`), and the schema stays
/// bounded (the size bound fails when the cap is removed).
#[test]
fn polymorphic_recursion_is_capped_per_top_level_instance() {
    take_materialized();
    let known: HashSet<String> = HashSet::from(["Nested".to_string()]);
    let defs = HashMap::from([(
        "Nested".to_string(),
        "struct Nested<T> { a: Vec<Nested<Vec<T>>>, b: Vec<Nested<Vec<T>>> }".to_string(),
    )]);
    let ty: Type = syn::parse_str("Nested<u8>").unwrap();
    let schema = serde_json::to_string(&parse_type_to_schema_ref(&ty, &known, &defs)).unwrap();
    assert!(schema.contains("{}"), "the cut instances are any value");
    assert!(
        schema.len() < 2_000_000,
        "the expansion is bounded: {} bytes",
        schema.len()
    );
}

/// `serde_json::Map` (any path through a `serde_json` segment) is a JSON
/// object of any members, not an unknown named type.
#[test]
fn serde_json_map_is_an_object_of_any_members() {
    take_materialized();
    let known: HashSet<String> = HashSet::new();
    let defs: HashMap<String, String> = HashMap::new();
    for src in [
        "serde_json::Map<String, serde_json::Value>",
        "vespera::serde_json::Map<String, u8>",
        "::serde_json::Map",
    ] {
        let ty: Type = syn::parse_str(src).unwrap();
        let schema = serde_json::to_value(parse_type_to_schema_ref(&ty, &known, &defs)).unwrap();
        assert_eq!(
            schema,
            serde_json::json!({"type": "object", "additionalProperties": true}),
            "{src}"
        );
    }
    assert!(take_materialized().is_empty(), "no component for a builtin");
}

/// A use site that omits a defaulted parameter (`Page<User>` for `Page<T, M =
/// Meta>`) is the instance that spells it (`Page<User, Meta>`): one component,
/// named with the default filled in (`Page_User_Meta`), never a second one.
#[test]
fn omitted_defaulted_argument_names_the_spelled_component() {
    take_materialized();
    let src = "struct Page<T, M = Meta> { value: T, meta: M, kids: Vec<Page<T>> }";
    let known: HashSet<String> = ["Page", "Meta", "User"]
        .into_iter()
        .map(String::from)
        .collect();
    let defs = HashMap::from([
        ("Page".to_string(), src.to_string()),
        ("Meta".to_string(), "struct Meta { x: u8 }".to_string()),
        ("User".to_string(), "struct User { id: u8 }".to_string()),
    ]);
    let reference_of = |src: &str| {
        let ty: Type = syn::parse_str(src).unwrap();
        reference(parse_type_to_schema_ref(&ty, &known, &defs))
    };
    assert_eq!(
        reference_of("Page<User>"),
        "#/components/schemas/Page_User_Meta"
    );
    assert_eq!(
        reference_of("Page<User, Meta>"),
        "#/components/schemas/Page_User_Meta"
    );
    assert_eq!(
        take_materialized().keys().collect::<Vec<_>>(),
        ["Page_User_Meta"]
    );
}
