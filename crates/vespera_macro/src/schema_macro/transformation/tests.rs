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
    r#"#[serde(default = "f", rename = "e-mail", alias = "mail")]"#,
    true,
    false
)]
#[case::partial_skip_if_becomes_is_none(
    "field_partial_skip_if",
    r#"#[serde(skip_serializing_if = "String::is_empty", default)]"#,
    true,
    false
)]
#[case::partial_with_module(
    "field_partial_with_module",
    r#"#[serde(default = "f", with = "codec::as_string", rename = "e-mail")]"#,
    true,
    false
)]
#[case::partial_split_codec(
    "field_partial_split_codec",
    r#"#[serde(serialize_with = "s")] #[serde(deserialize_with = "d", default)]"#,
    true,
    false
)]
#[case::partial_serialize_only(
    "field_partial_serialize_only",
    r#"#[serde(serialize_with = "s", skip_serializing_if = "is_zero")]"#,
    true,
    false
)]
#[case::partial_non_string_codec_dropped(
    "field_partial_non_string_codec",
    r"#[serde(with = 42, alias = 'x')]",
    true,
    false
)]
#[case::partial_valueless_codec_dropped(
    "field_partial_valueless_codec",
    r#"#[serde(serialize_with, deserialize_with(x), alias = "y")]"#,
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
    let inner_ty = quote!(T);
    let wrap = OptionWrap {
        adapter_prefix: "__vespera_Dto_x",
        inner_ty: &inner_ty,
    };
    let (attrs, adapters) = field_serde_attrs(&attrs, wrapped.then_some(&wrap), strip_rename);
    let mut rendered = render(&attrs);
    if !adapters.is_empty() {
        let file: syn::File = syn::parse2(adapters).unwrap();
        rendered.push_str("\n---\n");
        rendered.push_str(&prettyplease::unparse(&file));
    }
    insta::assert_snapshot!(name, rendered);
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
#[case::tag_with_source_rename(
    "container_tag_source_rename",
    r#"#[serde(tag = "t")] #[serde(rename = "wire")]"#
)]
#[case::conversions(
    "container_conversions",
    r#"#[serde(from = "W", into = "W")] #[serde(try_from = "W")] #[doc = "d"] #[serde(bound = "")]"#
)]
fn container_serde_attrs_cases(#[case] name: &str, #[case] struct_attrs: &str) {
    let item: syn::ItemStruct = syn::parse_str(&format!("{struct_attrs} struct S {{}}")).unwrap();
    insta::assert_snapshot!(name, render(&container_serde_attrs(&item.attrs, "S")));
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
        renamed_field_serde_rename(
            &attrs,
            source,
            &RenameAll::new(Some(&rename_all.to_string()), &[], rename_all)
        )
        .to_string(),
        expected
    );
}

#[rstest]
#[case::rename_all_applied(r"", true, Some(r#"# [form_data (field_name = "filePath")]"#))]
#[case::other_form_data(
    r#"#[form_data(limit = "1MiB")]"#,
    true,
    Some(r#"# [form_data (field_name = "filePath")]"#)
)]
#[case::explicit_field_name(r#"#[form_data(field_name = "doc")]"#, true, None)]
#[case::serde_rename_before_rename_all(
    r#"#[serde(rename = "upload")]"#,
    true,
    Some(r#"# [form_data (field_name = "upload")]"#)
)]
#[case::form_data_before_serde_rename(
    r#"#[serde(rename = "upload")] #[form_data(field_name = "doc")]"#,
    true,
    None
)]
#[case::kept_field_serde_rename(
    r#"#[serde(rename = "upload")]"#,
    false,
    Some(r#"# [form_data (field_name = "upload")]"#)
)]
#[case::kept_field_plain(r"", false, None)]
#[case::deserialize_side_rename(
    r#"#[serde(rename(serialize = "out", deserialize = "in"))]"#,
    false,
    Some(r#"# [form_data (field_name = "in")]"#)
)]
fn field_form_name_cases(
    #[case] field_attrs: &str,
    #[case] renamed: bool,
    #[case] expected: Option<&str>,
) {
    let attrs = parse_attrs(field_attrs);
    assert_eq!(
        field_form_name(&attrs, "file_path", "camelCase", renamed).map(|t| t.to_string()),
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

#[rstest]
#[case::plain(
    r#"#[serde(rename_all = "snake_case")]"#,
    None,
    "# [serde (rename_all = \"snake_case\")]"
)]
#[case::split(
    r#"#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]"#,
    None,
    "# [serde (rename_all (serialize = \"camelCase\" , deserialize = \"snake_case\"))]"
)]
#[case::serialize_only(
    r#"#[serde(rename_all(serialize = "kebab-case"))]"#,
    None,
    "# [serde (rename_all (serialize = \"kebab-case\" ,))]"
)]
#[case::deserialize_only(
    r#"#[serde(rename_all(deserialize = "kebab-case"))]"#,
    None,
    "# [serde (rename_all (deserialize = \"kebab-case\"))]"
)]
#[case::input_wins(
    r#"#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]"#,
    Some("PascalCase"),
    "# [serde (rename_all = \"PascalCase\")]"
)]
fn rename_all_keeps_both_sides(
    #[case] container: &str,
    #[case] input: Option<&str>,
    #[case] expected: &str,
) {
    let attrs = parse_attrs(container);
    let input = input.map(ToString::to_string);
    let effective = determine_rename_all(input.as_ref(), &attrs);
    let rename_all = RenameAll::new(input.as_ref(), &attrs, &effective);
    assert_eq!(rename_all.attr().to_string(), expected);
}

#[test]
fn renamed_field_follows_each_rename_all_side() {
    let rename_all = RenameAll {
        serialize: Some("camelCase".to_string()),
        deserialize: None,
    };
    assert_eq!(
        renamed_field_serde_rename(&[], "user_name", &rename_all).to_string(),
        "# [serde (rename (serialize = \"userName\" , deserialize = \"user_name\"))]"
    );
}

/// M1: serde function paths of a source in another module are re-rooted at
/// that module (`crate::models::user::default_name`), unknown / absolute /
/// non-path values stay as written, non-serde attributes are untouched.
#[rstest]
#[case::local_fn(
    r#"#[serde(default = "default_name")]"#,
    r#"#[serde(default = "crate::models::user::default_name")]"#
)]
#[case::local_module(
    r#"#[serde(with = "codecs::as_string")]"#,
    r#"#[serde(with = "crate::models::user::codecs::as_string")]"#
)]
#[case::imported_fn(
    r#"#[serde(serialize_with = "ser")]"#,
    r#"#[serde(serialize_with = "crate::util::ser")]"#
)]
#[case::all_keys(
    r#"#[serde(rename = "x", skip_serializing_if = "is_blank", deserialize_with = "codecs::de")]"#,
    r#"#[serde(rename = "x", skip_serializing_if = "crate::models::user::is_blank", deserialize_with = "crate::models::user::codecs::de")]"#
)]
#[case::std_kept(
    r#"#[serde(skip_serializing_if = "Option::is_none")]"#,
    r#"#[serde(skip_serializing_if = "Option::is_none")]"#
)]
#[case::leading_colon_kept(
    r#"#[serde(default = "::other::f")]"#,
    r#"#[serde(default = "::other::f")]"#
)]
#[case::not_a_path_kept(r#"#[serde(default = "1 + 2")]"#, r#"#[serde(default = "1 + 2")]"#)]
#[case::flag_kept(r#"#[serde(default)]"#, r#"#[serde(default)]"#)]
#[case::non_string_kept(r#"#[serde(default = 1)]"#, r#"#[serde(default = 1)]"#)]
#[case::unparseable_kept(r#"#[serde(default = )]"#, r#"#[serde(default = )]"#)]
#[case::other_attr(
    r#"#[schema(default = "default_name")]"#,
    r#"#[schema(default = "default_name")]"#
)]
fn test_reroot_serde_paths(#[case] attr: &str, #[case] expected: &str) {
    let (_dir, _guard) = serde_path_crate();
    let attrs: Vec<syn::Attribute> = vec![
        syn::parse_str::<syn::DeriveInput>(&format!("{attr} struct S;"))
            .unwrap()
            .attrs
            .remove(0),
    ];
    let module = vec![
        "crate".to_string(),
        "models".to_string(),
        "user".to_string(),
    ];
    let rerooted = reroot_serde_paths(&attrs, &module);
    let expected = syn::parse_str::<syn::DeriveInput>(&format!("{expected} struct S;"))
        .unwrap()
        .attrs
        .remove(0);
    assert_eq!(
        quote!(#(#rerooted)*).to_string(),
        quote!(#expected).to_string()
    );
}

fn serde_path_crate() -> (tempfile::TempDir, crate::env_access::test_env::EnvGuard) {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models; pub mod util;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod user;").unwrap();
    std::fs::write(src.join("util.rs"), "pub fn ser() {}").unwrap();
    std::fs::write(
        src.join("models").join("user.rs"),
        "use crate::util::ser; fn default_name() {} fn is_blank() {} mod codecs { pub mod as_string {} pub fn de() {} }",
    )
    .unwrap();
    let guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    super::super::file_cache::bump_epoch();
    (dir, guard)
}
