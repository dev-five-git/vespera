//! Unit tests for `request_view` (sidecar module; `use super::*` semantics
//! preserved).

use rstest::rstest;

use super::*;

fn names(sources: &[(&str, Option<&str>, &str)], taken: &[&str]) -> Vec<(String, String)> {
    let sources: Vec<_> = sources
        .iter()
        .map(|&(name, ident, src)| (name, ident, None, src))
        .collect();
    scoped_names(&sources, taken)
}

/// `names` with a `module::Ident` key per source.
fn scoped_names(
    sources: &[(&str, Option<&str>, Option<&str>, &str)],
    taken: &[&str],
) -> Vec<(String, String)> {
    let items: Vec<syn::Item> = sources
        .iter()
        .map(|(_, _, _, src)| syn::parse_str(src).unwrap())
        .collect();
    let views: Vec<ViewSource<'_>> = sources
        .iter()
        .zip(&items)
        .map(|((name, ident, scoped_key, _), item)| ViewSource {
            name,
            rust_ident: *ident,
            scoped_key: *scoped_key,
            item,
        })
        .collect();
    let mut taken: BTreeSet<String> = taken.iter().map(ToString::to_string).collect();
    taken.extend(sources.iter().map(|(name, _, _, _)| name.to_string()));
    compute_input_names(&views, &taken)
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[rstest]
#[case::plain("struct A { x: i32, y: Option<u8> }", false)]
#[case::flatten_option("struct A { #[serde(flatten)] p: Option<Page> }", true)]
#[case::flatten_map_then_struct(
    "struct A { #[serde(flatten)] m: HashMap<String, u8>, #[serde(flatten)] p: Page }",
    true
)]
#[case::flatten_struct_then_map(
    "struct A { #[serde(flatten)] p: Page, #[serde(flatten)] m: BTreeMap<String, u8> }",
    false
)]
#[case::flatten_non_path("struct A { #[serde(flatten)] p: (Page), q: u8 }", false)]
// Written (the response lists the map) but never read (closed request).
#[case::deny_flatten_map(
    "#[serde(deny_unknown_fields)] struct A { id: u8, #[serde(flatten)] m: HashMap<String, u8> }",
    true
)]
#[case::deny_flatten_map_bearing(
    "#[serde(deny_unknown_fields)] struct A { #[serde(flatten)] h: HasMap }",
    true
)]
#[case::deny_flatten_struct(
    "#[serde(deny_unknown_fields)] struct A { #[serde(flatten)] p: Page }",
    false
)]
// `HasMap` flattens a map, which also reads `Page`'s keys.
#[case::flatten_map_bearing_then_struct(
    "struct A { #[serde(flatten)] h: HasMap, #[serde(flatten)] p: Page }",
    true
)]
#[case::enum_variant_flatten_map_then_struct(
    "enum E { V { #[serde(flatten)] m: HashMap<String, u8>, #[serde(flatten)] p: Page } }",
    true
)]
#[case::enum_deny_variant_flatten_map(
    "#[serde(deny_unknown_fields)] enum E { V { #[serde(flatten)] m: HashMap<String, u8> } }",
    true
)]
#[case::enum_variant_flatten_struct("enum E { V { #[serde(flatten)] p: Page } }", false)]
#[case::field_default("struct A { #[serde(default)] x: i32 }", true)]
#[case::container_default("#[serde(default)] struct A { x: i32 }", true)]
#[case::skip_deserializing("struct A { #[serde(skip_deserializing)] id: i32 }", true)]
#[case::skip_serializing("struct A { #[serde(skip_serializing)] pw: String }", true)]
#[case::fully_skipped("struct A { #[serde(skip)] cache: u8, x: i32 }", false)]
#[case::sided_rename(r#"struct A { #[serde(rename(deserialize = "in"))] x: i32 }"#, true)]
#[case::same_rename(r#"struct A { #[serde(rename = "X")] x: i32 }"#, false)]
#[case::sided_rename_all(
    r#"#[serde(rename_all(deserialize = "camelCase"))] struct A { a_b: i32 }"#,
    true
)]
// Optional on output, but serde reads it as a required key.
#[case::skip_serializing_if(
    r#"struct A { #[serde(skip_serializing_if = "Vec::is_empty")] v: Vec<u8> }"#,
    true
)]
#[case::skip_serializing_if_option(
    r#"struct A { #[serde(skip_serializing_if = "Option::is_none")] v: Option<u8> }"#,
    false
)]
#[case::alias(r#"struct A { #[serde(alias = "b")] a: u8 }"#, true)]
#[case::hash_set("struct A { tags: std::collections::HashSet<String> }", true)]
#[case::btree_set_value_type("struct A { #[schema(value_type = BTreeSet<u8>)] tags: Bits }", true)]
#[case::std_duration("struct A { ttl: std::time::Duration }", true)]
#[case::bare_duration("struct A { ttl: Duration }", false)]
#[case::empty_container_default("#[serde(default)] struct A {}", false)]
#[case::tuple("struct A(#[serde(default)] u8);", false)]
#[case::tuple_one_sided_skip("struct A(u8, #[serde(skip_serializing)] u8);", true)]
#[case::tuple_both_skips("struct A(u8, #[serde(skip)] u8);", false)]
#[case::tuple_one_sided_codec(r#"struct A(#[serde(serialize_with = "f")] u8);"#, true)]
#[case::tuple_set("struct A(HashSet<u8>);", true)]
#[case::tuple_position_default("struct A(u32, #[serde(default)] u32);", true)]
#[case::tuple_container_default("#[serde(default)] struct A(u32, u32);", true)]
#[case::newtype_container_default("#[serde(default)] struct A(u32);", false)]
#[case::tuple_skipped_default("struct A(u8, #[serde(skip, default)] u8);", false)]
#[case::enum_tuple_default("enum E { P(u8, #[serde(default)] u8) }", true)]
#[case::enum_adjacent_option(
    r#"#[serde(tag = "k", content = "d")] enum E { A(Option<String>), B(String) }"#,
    true
)]
#[case::enum_adjacent_option_deserialize_with(
    r#"#[serde(tag = "k", content = "d")] enum E { #[serde(with = "m")] A(Option<u8>) }"#,
    false
)]
#[case::enum_adjacent_plain(r#"#[serde(tag = "k", content = "d")] enum E { A(u8) }"#, false)]
#[case::enum_external_option("enum E { A(Option<String>) }", false)]
#[case::unit_struct("struct A;", false)]
#[case::enum_variant_alias(r#"enum E { #[serde(alias = "a")] A, B }"#, true)]
#[case::enum_other(r"enum E { A, #[serde(other)] Unknown }", true)]
#[case::enum_variant_one_sided_codec(r#"enum E { #[serde(deserialize_with = "f")] A(u8) }"#, true)]
#[case::enum_variant_with_both(r#"enum E { #[serde(with = "m")] A(u8) }"#, false)]
#[case::enum_tuple_one_sided_skip("enum E { A(u8, #[serde(skip_deserializing)] u8) }", true)]
#[case::enum_variant_field_default("enum E { A { #[serde(default)] x: u8 } }", true)]
#[case::enum_plain("enum E { B(u8), C { x: u8 } }", false)]
#[case::enum_sided_rename_all(
    r#"#[serde(rename_all(deserialize = "lowercase"))] enum E { A }"#,
    true
)]
#[case::enum_same_rename_all(r#"#[serde(rename_all = "lowercase")] enum E { A }"#, false)]
#[case::enum_sided_rename_all_fields(
    r#"#[serde(rename_all_fields(serialize = "camelCase"))] enum E { A { x_y: u8 } }"#,
    true
)]
#[case::enum_variant_sided_rename(r#"enum E { #[serde(rename(deserialize = "a"))] A }"#, true)]
#[case::enum_variant_skip_serializing("enum E { A, #[serde(skip_serializing)] B }", true)]
#[case::enum_variant_skip_deserializing("enum E { A, #[serde(skip_deserializing)] B }", true)]
#[case::enum_variant_skipped("enum E { A, #[serde(skip)] B }", false)]
#[case::enum_serialize_repr_only("#[derive(Serialize_repr)] #[repr(u8)] enum E { A }", true)]
#[case::enum_serde_repr_both(
    "#[derive(Serialize_repr, Deserialize_repr)] #[repr(u8)] enum E { A }",
    false
)]
#[case::into_only(r#"#[serde(into = "W")] struct A { x: u8 }"#, true)]
#[case::from_only(r#"#[serde(from = "W")] struct A(u8);"#, true)]
#[case::into_and_from_same(r#"#[serde(into = "W", from = "W")] struct A { x: u8 }"#, false)]
#[case::container_value_type(
    r#"#[schema(value_type = W)] #[serde(into = "X")] struct A { x: u8 }"#,
    false
)]
#[case::enum_into(r#"#[serde(into = "String")] enum E { A }"#, true)]
#[case::with_both_sides(r#"struct A { #[serde(with = "m")] x: u8 }"#, false)]
#[case::serialize_with(r#"struct A { #[serde(serialize_with = "f")] x: u8 }"#, true)]
#[case::deserialize_with_value_type(
    r#"struct A { #[serde(deserialize_with = "f")] #[schema(value_type = u8)] x: u8 }"#,
    false
)]
#[case::option_with(r#"struct A { #[serde(with = "m")] x: Option<u8> }"#, true)]
#[case::struct_tag(r#"#[serde(tag = "type")] struct A { x: u8 }"#, true)]
#[case::struct_tag_no_fields(r#"#[serde(tag = "type")] struct A {}"#, true)]
#[case::struct_tag_value_type(
    r#"#[schema(value_type = W)] #[serde(tag = "type")] struct A { x: u8 }"#,
    false
)]
#[case::struct_tag_into(r#"#[serde(tag = "type", into = "W")] struct A { x: u8 }"#, true)]
#[case::form_data_over_serde_rename(
    r#"struct A { #[serde(rename = "a")] #[form_data(field_name = "b")] x: u8 }"#,
    true
)]
#[case::form_data_only(r#"struct A { #[form_data(field_name = "b")] x: u8 }"#, false)]
#[case::mixed_external_enum("enum Shape { Point, Square(u8) }", true)]
#[case::mixed_struct_variant_enum("enum Shape { Point, Rect { w: u8 } }", true)]
#[case::all_unit_enum("enum Color { Red, Green }", false)]
#[case::all_data_enum("enum E { A(u8), B { x: u8 } }", false)]
#[case::mixed_unit_skipped_on_input(
    "enum Shape { #[serde(skip_deserializing)] Point, Square(u8) }",
    true
)]
#[case::mixed_unit_fully_skipped("enum Shape { #[serde(skip)] Point, Square(u8) }", false)]
#[case::mixed_untagged_unit("enum Shape { #[serde(untagged)] Point, Square(u8) }", false)]
#[case::mixed_internally_tagged(
    r#"#[serde(tag = "t")] enum Shape { Point, Rect { w: u8 } }"#,
    false
)]
#[case::tuple_skip_serializing_if(
    r#"struct P(u8, #[serde(skip_serializing_if = "Option::is_none")] Option<u8>);"#,
    true
)]
#[case::tuple_variant_skip_serializing_if(
    r#"enum E { V(u8, #[serde(skip_serializing_if = "Option::is_none")] Option<u8>) }"#,
    true
)]
#[case::newtype_skip_serializing_if(
    r#"struct P(#[serde(skip_serializing_if = "Option::is_none")] Option<u8>);"#,
    false
)]
#[case::decimal_field("struct A { price: Decimal }", true)]
#[case::decimal_tuple("struct A(rust_decimal::Decimal);", true)]
fn direct_differences(#[case] src: &str, #[case] expected: bool) {
    let map_bearing = BTreeSet::from(["HasMap".to_string()]);
    assert_eq!(
        directly_differs(&syn::parse_str(src).unwrap(), &map_bearing),
        expected
    );
}

/// Structs flattening a map — directly, through an `Option`, or through
/// another such struct — read every buffered key.
#[test]
fn map_bearing_structs_are_transitive() {
    let items: Vec<syn::Item> = [
        "struct Outer { #[serde(flatten)] h: Option<HasMap> }",
        "struct HasMap { #[serde(flatten)] m: HashMap<String, u8> }",
        "struct Plain { #[serde(flatten)] p: Page, m: HashMap<String, u8> }",
        "struct Tuple(HashMap<String, u8>);",
        "enum E { V { #[serde(flatten)] m: HashMap<String, u8> } }",
    ]
    .iter()
    .map(|src| syn::parse_str(src).unwrap())
    .collect();
    let items: Vec<&syn::Item> = items.iter().collect();
    assert_eq!(
        map_bearing_structs(&items),
        BTreeSet::from(["HasMap".to_string(), "Outer".to_string()])
    );
}

#[test]
fn differences_propagate_and_names_avoid_collisions() {
    let result = names(
        &[
            ("Line", None, "struct Line { #[serde(default)] qty: u32 }"),
            ("Order", None, "struct Order { lines: Vec<Line> }"),
            ("Wrapper", None, "enum Wrapper { One(Order), Two }"),
            ("Plain", None, "struct Plain { x: u8 }"),
            (
                "UserDto",
                Some("User"),
                "struct User { #[serde(default)] x: u8 }",
            ),
        ],
        &["LineInput", "LineInput2"],
    );
    assert_eq!(
        result,
        [
            ("Line".into(), "LineInput3".into()),
            ("Order".into(), "OrderInput".into()),
            ("User".into(), "UserDtoInput".into()),
            ("UserDto".into(), "UserDtoInput".into()),
            ("Wrapper".into(), "WrapperInput".into()),
        ]
    );
}

#[test]
fn ident_alias_never_shadows_a_real_component() {
    // `User` is a real component; the renamed `UserDto` (ident `User`)
    // must not claim the `User` key.
    let result = names(
        &[
            ("User", None, "struct User { x: u8 }"),
            (
                "UserDto",
                Some("User"),
                "struct User { #[serde(default)] x: u8 }",
            ),
        ],
        &[],
    );
    assert_eq!(result, [("UserDto".into(), "UserDtoInput".into())]);
}

/// Two renames share the ident `Foo`; the bare `Foo` aliases `A`. A
/// module-qualified field propagates the input view of the type its
/// module declares, in both directions.
#[rstest]
#[case::qualified_differs(
    "struct Foo { #[serde(skip_deserializing)] b: u8 }",
    "struct Foo { a: u8 }",
    "b::Foo",
    true
)]
#[case::bare_alias_differs(
    "struct Foo { b: u8 }",
    "struct Foo { #[serde(skip_deserializing)] a: u8 }",
    "b::Foo",
    false
)]
#[case::unregistered_module(
    "struct Foo { b: u8 }",
    "struct Foo { #[serde(skip_deserializing)] a: u8 }",
    "c::Foo",
    true
)]
fn qualified_references_resolve_their_module(
    #[case] b_src: &str,
    #[case] a_src: &str,
    #[case] field_ty: &str,
    #[case] outer_differs: bool,
) {
    let outer = format!("struct Outer {{ f: {field_ty} }}");
    let result = scoped_names(
        &[
            ("A", Some("Foo"), Some("a::Foo"), a_src),
            ("B", Some("Foo"), Some("b::Foo"), b_src),
            ("Outer", None, None, &outer),
        ],
        &[],
    );
    assert_eq!(
        result.iter().any(|(component, _)| component == "Outer"),
        outer_differs,
        "{result:?}"
    );
    if outer_differs && field_ty == "b::Foo" {
        assert!(
            result.contains(&("b::Foo".into(), "BInput".into())),
            "{result:?}"
        );
        install(Arc::new(result.into_iter().collect()));
        let types: [syn::Type; 1] = [syn::parse_quote!(Vec<b::Foo>)];
        assert!(mentions_input(&types));
        assert_eq!(component_of_input("BInput").as_deref(), Some("B"));
        install(Arc::default());
    }
}

#[test]
fn view_mode_and_installed_names() {
    install(Arc::new(BTreeMap::from([(
        "A".to_string(),
        "AInput".to_string(),
    )])));
    assert!(!in_request_view());
    assert_eq!(input_component("A"), None);
    assert!(has_input("A"));
    assert_eq!(component_of_input("AInput").as_deref(), Some("A"));
    assert_eq!(component_of_input("A"), None);
    let inner = with_request_view(|| {
        assert!(in_request_view());
        (input_component("A"), input_component("B"))
    });
    assert_eq!(inner, (Some("AInput".to_string()), None));
    assert!(!in_request_view());
    assert_eq!(snapshot().len(), 1);
    let types: [syn::Type; 2] = [syn::parse_quote!(Vec<A>), syn::parse_quote!(u8)];
    assert!(mentions_input(&types));
    assert!(!mentions_input(&types[1..]));
    install(Arc::default());
    assert!(!has_input("A"));
}

#[rstest]
#[case::field_value_type(
        "struct A { #[schema(value_type = Vec<W>)] x: u8 }",
        &["Vec", "W", "u8"]
    )]
#[case::struct_conversions(
        r#"#[serde(into = "Out", try_from = "In")] struct A { x: u8 }"#,
        &["In", "Out", "u8"]
    )]
#[case::enum_conversion(
        r#"#[serde(from = "Raw")] enum E { V(Payload) }"#,
        &["Payload", "Raw"]
    )]
fn referenced_idents_cover_overrides(#[case] src: &str, #[case] expected: &[&str]) {
    let item: syn::Item = syn::parse_str(src).unwrap();
    let mut expected: Vec<&str> = expected.to_vec();
    expected.sort_unstable();
    let found = referenced_idents(&item);
    assert_eq!(
        found.iter().map(String::as_str).collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn container_conversion_propagates_input_views() {
    // `Order` deserializes from `RawOrder`, whose request view differs;
    // `Order` itself only names `u8` fields.
    let result = names(
        &[
            (
                "RawOrder",
                None,
                "struct RawOrder { #[serde(default)] qty: u32 }",
            ),
            (
                "Order",
                None,
                r"#[schema(value_type = RawOrder)] struct Order { qty: u8 }",
            ),
        ],
        &[],
    );
    assert_eq!(
        result,
        [
            ("Order".into(), "OrderInput".into()),
            ("RawOrder".into(), "RawOrderInput".into()),
        ]
    );
}

#[test]
fn current_side_follows_the_view() {
    assert_eq!(current_side(), SerdeSide::Serialize);
    assert_eq!(with_request_view(current_side), SerdeSide::Deserialize);
}

#[test]
fn non_type_items_reference_nothing() {
    let item: syn::Item = syn::parse_str("fn f() {}").unwrap();
    assert!(referenced_idents(&item).is_empty());
    assert!(!directly_differs(&item, &BTreeSet::new()));
}
