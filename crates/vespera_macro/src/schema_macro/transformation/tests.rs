use rstest::rstest;

use super::*;

#[test]
fn test_build_omit_set() {
    let omit = Some(vec!["password".to_string(), "secret".to_string()]);
    let set = build_omit_set(omit.as_ref());

    assert!(set.contains("password"));
    assert!(set.contains("secret"));
    assert_eq!(set.len(), 2);
}

#[test]
fn test_build_omit_set_none() {
    let set = build_omit_set(None);
    assert!(set.is_empty());
}

#[test]
fn test_build_pick_set() {
    let pick = Some(vec!["id".to_string(), "name".to_string()]);
    let set = build_pick_set(pick.as_ref());

    assert!(set.contains("id"));
    assert!(set.contains("name"));
    assert_eq!(set.len(), 2);
}

#[test]
fn test_build_partial_config_all() {
    let partial = Some(PartialMode::All);
    let (all, set) = build_partial_config(&partial);

    assert!(all);
    assert!(set.is_empty());
}

#[test]
fn test_build_partial_config_fields() {
    let partial = Some(PartialMode::Fields(vec![
        "name".to_string(),
        "email".to_string(),
    ]));
    let (all, set) = build_partial_config(&partial);

    assert!(!all);
    assert!(set.contains("name"));
    assert!(set.contains("email"));
}

#[test]
fn test_build_partial_config_none() {
    let (all, set) = build_partial_config(&None);

    assert!(!all);
    assert!(set.is_empty());
}

#[test]
fn test_build_rename_map() {
    let rename = Some(vec![
        ("id".to_string(), "user_id".to_string()),
        ("name".to_string(), "full_name".to_string()),
    ]);
    let map = build_rename_map(rename.as_ref());

    assert_eq!(map.get("id"), Some(&"user_id".to_string()));
    assert_eq!(map.get("name"), Some(&"full_name".to_string()));
}

#[test]
fn test_build_rename_map_none() {
    let map = build_rename_map(None);
    assert!(map.is_empty());
}

#[test]
fn test_extract_doc_attrs() {
    let attrs: Vec<syn::Attribute> = vec![
        syn::parse_quote!(#[doc = "First doc"]),
        syn::parse_quote!(#[serde(default)]),
        syn::parse_quote!(#[doc = "Second doc"]),
    ];

    let docs = extract_doc_attrs(&attrs);

    assert_eq!(docs.len(), 2);
}

#[test]
fn test_determine_rename_all_with_input() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(rename_all = "snake_case")])];

    let result = determine_rename_all(Some(&"PascalCase".to_string()), &attrs);

    assert_eq!(result, "PascalCase");
}

#[test]
fn test_determine_rename_all_from_source() {
    let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[serde(rename_all = "snake_case")])];

    let result = determine_rename_all(None, &attrs);

    assert_eq!(result, "snake_case");
}

#[test]
fn test_determine_rename_all_default() {
    let attrs: Vec<syn::Attribute> = vec![];

    let result = determine_rename_all(None, &attrs);

    assert_eq!(result, "camelCase");
}

fn render(attrs: &[TokenStream]) -> String {
    attrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_attrs(field: &str) -> Vec<syn::Attribute> {
    syn::parse_str::<syn::ItemStruct>(&format!("struct S {{ {field} x: T }}"))
        .unwrap()
        .fields
        .into_iter()
        .next()
        .unwrap()
        .attrs
}

#[rstest]
#[case::plain_field(
    "field_plain",
    r#"#[serde(rename = "id", default = "f")] #[doc = "d"]"#,
    false,
    false
)]
#[case::strip_rename_keeps_siblings(
    "field_strip_rename",
    r#"#[serde(default = "f", rename = "userId", alias = "uid")]"#,
    false,
    true
)]
#[case::only_rename_dropped(
    "field_only_rename_dropped",
    r#"#[serde(rename = "userId")]"#,
    false,
    true
)]
#[case::partial_drops_typed_keys(
    "field_partial_typed_keys",
    r#"#[serde(default = "f", with = "m", rename = "e-mail", alias = "mail")]
       #[serde(serialize_with = "s", deserialize_with = "d")]"#,
    true,
    false
)]
#[case::partial_skip_if_becomes_is_none(
    "field_partial_skip_if",
    r#"#[serde(skip_serializing_if = "String::is_empty", default)]"#,
    true,
    false
)]
#[case::unparseable_kept_verbatim("field_unparseable", "#[serde(rename = )]", true, false)]
fn field_serde_attrs_cases(
    #[case] name: &str,
    #[case] field_attrs: &str,
    #[case] wrapped: bool,
    #[case] strip_rename: bool,
) {
    let attrs = parse_attrs(field_attrs);
    insta::assert_snapshot!(
        name,
        render(&field_serde_attrs(&attrs, wrapped, strip_rename))
    );
}

#[rstest]
#[case::default_and_rename_all(
    "container_default_rename_all",
    r#"#[serde(default, deny_unknown_fields, rename_all = "camelCase")]"#
)]
#[case::tag_before_rename_all(
    "container_tag_rename_all",
    r#"#[serde(tag = "t", rename_all = "snake_case")]"#
)]
#[case::conversions(
    "container_conversions",
    r#"#[serde(from = "W", into = "W")] #[serde(try_from = "W")] #[doc = "d"] #[serde(bound = "")]"#
)]
fn container_serde_attrs_cases(#[case] name: &str, #[case] struct_attrs: &str) {
    let item: syn::ItemStruct = syn::parse_str(&format!("{struct_attrs} struct S {{}}")).unwrap();
    insta::assert_snapshot!(name, render(&container_serde_attrs(&item.attrs)));
}

#[rstest]
#[case::rename_all_applied(r"", "user_name", "camelCase", r#"# [serde (rename = "userName")]"#)]
#[case::explicit_rename_wins(
    r#"#[serde(rename = "login")]"#,
    "user_name",
    "camelCase",
    r#"# [serde (rename = "login")]"#
)]
#[case::sided_rename(
    r#"#[serde(rename(deserialize = "in"))]"#,
    "user_name",
    "snake_case",
    r#"# [serde (rename (serialize = "user_name" , deserialize = "in"))]"#
)]
#[case::form_data_ignored(
    r#"#[form_data(field_name = "x")]"#,
    "a_b",
    "kebab-case",
    r#"# [serde (rename = "a-b")]"#
)]
fn renamed_field_serde_rename_cases(
    #[case] field_attrs: &str,
    #[case] source: &str,
    #[case] rename_all: &str,
    #[case] expected: &str,
) {
    let attrs = parse_attrs(field_attrs);
    assert_eq!(
        renamed_field_serde_rename(&attrs, source, rename_all).to_string(),
        expected
    );
}

#[rstest]
#[case::rename_all_applied(r"", Some(r#"# [form_data (field_name = "filePath")]"#))]
#[case::other_form_data(
    r#"#[form_data(limit = "1MiB")]"#,
    Some(r#"# [form_data (field_name = "filePath")]"#)
)]
#[case::explicit_field_name(r#"#[form_data(field_name = "doc")]"#, None)]
fn renamed_field_form_name_cases(#[case] field_attrs: &str, #[case] expected: Option<&str>) {
    let attrs = parse_attrs(field_attrs);
    assert_eq!(
        renamed_field_form_name(&attrs, "file_path", "camelCase").map(|t| t.to_string()),
        expected.map(str::to_string)
    );
}

#[test]
fn test_should_skip_field_omit() {
    let omit_set: HashSet<String> = ["password".to_string()].into_iter().collect();
    let pick_set: HashSet<String> = HashSet::new();

    assert!(should_skip_field("password", &omit_set, &pick_set));
    assert!(!should_skip_field("name", &omit_set, &pick_set));
}

#[test]
fn test_should_skip_field_pick() {
    let omit_set: HashSet<String> = HashSet::new();
    let pick_set: HashSet<String> = ["id".to_string(), "name".to_string()].into_iter().collect();

    assert!(should_skip_field("email", &omit_set, &pick_set));
    assert!(!should_skip_field("id", &omit_set, &pick_set));
}

#[test]
fn test_should_skip_field_no_filters() {
    let omit_set: HashSet<String> = HashSet::new();
    let pick_set: HashSet<String> = HashSet::new();

    assert!(!should_skip_field("any_field", &omit_set, &pick_set));
}

#[test]
fn test_should_wrap_in_option_partial_all() {
    let partial_set: HashSet<String> = HashSet::new();

    assert!(should_wrap_in_option(
        "name",
        true,
        &partial_set,
        false,
        false
    ));
    assert!(!should_wrap_in_option(
        "name",
        true,
        &partial_set,
        true,
        false
    )); // already option
    assert!(!should_wrap_in_option(
        "rel",
        true,
        &partial_set,
        false,
        true
    )); // relation
}

#[test]
fn test_extract_form_data_attrs() {
    let attrs: Vec<syn::Attribute> = vec![
        syn::parse_quote!(#[form_data(limit = "10MiB")]),
        syn::parse_quote!(#[serde(default)]),
        syn::parse_quote!(#[doc = "Some doc"]),
        syn::parse_quote!(#[form_data(field_name = "my_file")]),
    ];

    let form_data = extract_form_data_attrs(&attrs);
    assert_eq!(form_data.len(), 2);
}

#[test]
fn test_extract_form_data_attrs_empty() {
    let attrs: Vec<syn::Attribute> = vec![
        syn::parse_quote!(#[serde(default)]),
        syn::parse_quote!(#[doc = "Some doc"]),
    ];

    let form_data = extract_form_data_attrs(&attrs);
    assert_eq!(form_data, Vec::<&syn::Attribute>::new());
}

#[test]
fn test_should_wrap_in_option_partial_fields() {
    let partial_set: HashSet<String> = ["name".to_string()].into_iter().collect();

    assert!(should_wrap_in_option(
        "name",
        false,
        &partial_set,
        false,
        false
    ));
    assert!(!should_wrap_in_option(
        "email",
        false,
        &partial_set,
        false,
        false
    ));
}
