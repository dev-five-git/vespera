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
    fn test_extract_serde_attrs_without_rename_all() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[serde(rename_all = "camelCase")]),
            syn::parse_quote!(#[serde(default)]),
            syn::parse_quote!(#[doc = "Some doc"]),
        ];

        let filtered = extract_serde_attrs_without_rename_all(&attrs);

        assert_eq!(filtered.len(), 1);
        // Should keep #[serde(default)] but not #[serde(rename_all = ...)]
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
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[serde(rename_all = "snake_case")])];

        let result = determine_rename_all(Some(&"PascalCase".to_string()), &attrs);

        assert_eq!(result, "PascalCase");
    }

    #[test]
    fn test_determine_rename_all_from_source() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[serde(rename_all = "snake_case")])];

        let result = determine_rename_all(None, &attrs);

        assert_eq!(result, "snake_case");
    }

    #[test]
    fn test_determine_rename_all_default() {
        let attrs: Vec<syn::Attribute> = vec![];

        let result = determine_rename_all(None, &attrs);

        assert_eq!(result, "camelCase");
    }

    #[test]
    fn test_extract_field_serde_attrs() {
        let attrs: Vec<syn::Attribute> = vec![
            syn::parse_quote!(#[serde(rename = "userId")]),
            syn::parse_quote!(#[doc = "The user ID"]),
            syn::parse_quote!(#[serde(default)]),
        ];

        let serde_attrs = extract_field_serde_attrs(&attrs);

        assert_eq!(serde_attrs.len(), 2);
    }

    #[test]
    #[allow(clippy::similar_names)]
    fn test_filter_out_serde_rename() {
        let attr1: syn::Attribute = syn::parse_quote!(#[serde(rename = "userId")]);
        let attr2: syn::Attribute = syn::parse_quote!(#[serde(default)]);
        let attrs: Vec<&syn::Attribute> = vec![&attr1, &attr2];

        let filtered = filter_out_serde_rename(&attrs);

        assert_eq!(filtered.len(), 1);
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
        let pick_set: HashSet<String> =
            ["id".to_string(), "name".to_string()].into_iter().collect();

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
        assert!(form_data.is_empty());
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
