use super::*;

#[test]
fn test_parse_schema_input_simple() {
    let tokens = quote::quote!(User);
    let input: SchemaInput = syn::parse2(tokens).unwrap();
    assert!(input.omit.is_none());
    assert!(input.pick.is_none());
}

#[test]
fn test_parse_schema_input_with_omit() {
    let tokens = quote::quote!(User, omit = ["password", "secret"]);
    let input: SchemaInput = syn::parse2(tokens).unwrap();
    let omit = input.omit.unwrap();
    assert_eq!(omit, vec!["password", "secret"]);
}

#[test]
fn test_parse_schema_input_with_pick() {
    let tokens = quote::quote!(User, pick = ["id", "name"]);
    let input: SchemaInput = syn::parse2(tokens).unwrap();
    let pick = input.pick.unwrap();
    assert_eq!(pick, vec!["id", "name"]);
}

#[test]
fn test_parse_empty_pick_is_rejected() {
    let err = syn::parse2::<SchemaInput>(quote::quote!(User, pick = []))
        .err()
        .expect("schema! pick = [] must be rejected");
    assert!(
        err.to_string().contains("`pick = []` selects no field"),
        "{err}"
    );
    let err = syn::parse2::<SchemaTypeInput>(quote::quote!(Empty from User, pick = []))
        .err()
        .expect("schema_type! pick = [] must be rejected");
    assert!(
        err.to_string().contains("`pick = []` selects no field"),
        "{err}"
    );
    // An empty `omit` keeps every field, as written.
    assert!(syn::parse2::<SchemaTypeInput>(quote::quote!(All from User, omit = [])).is_ok());
}

#[test]
fn test_parse_empty_add_is_rejected() {
    let err = syn::parse2::<SchemaTypeInput>(quote::quote!(Dto from User, add = []))
        .err()
        .expect("schema_type! add = [] must be rejected");
    assert!(
        err.to_string().contains("`add = []` adds no field"),
        "{err}"
    );
    // A non-empty `add` still parses.
    assert!(
        syn::parse2::<SchemaTypeInput>(quote::quote!(Dto from User, add = [("extra": i32)]))
            .is_ok()
    );
}

#[test]
fn test_parse_schema_input_omit_and_pick_error() {
    // Either order is rejected (at the second filter).
    for tokens in [
        quote::quote!(User, omit = ["a"], pick = ["b"]),
        quote::quote!(User, pick = ["a"], omit = ["b"]),
    ] {
        let Err(err) = syn::parse2::<SchemaInput>(tokens) else {
            panic!("omit + pick must be rejected");
        };
        assert!(err.to_string().contains("cannot use both"), "{err}");
    }
}

#[test]
fn test_parse_schema_input_trailing_comma() {
    let tokens = quote::quote!(User, omit = ["password"],);
    let input: SchemaInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.omit.unwrap(), vec!["password"]);
}

#[test]
fn test_parse_schema_input_unknown_param() {
    let tokens = quote::quote!(User, unknown = ["a"]);
    let result: syn::Result<SchemaInput> = syn::parse2(tokens);
    assert!(result.is_err());
    if let Err(e) = result {
        assert!(e.to_string().contains("unknown parameter"));
    }
}

#[test]
fn test_parse_schema_type_input_simple() {
    let tokens = quote::quote!(CreateUser from User);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.new_type.to_string(), "CreateUser");
    assert!(input.omit.is_none());
    assert!(input.pick.is_none());
    assert!(input.rename.is_none());
    assert_eq!(input.relation_adapters, Vec::<(String, syn::Ident)>::new());
    assert!(input.derive_clone);
}

#[test]
fn test_parse_schema_type_input_with_pick() {
    let tokens = quote::quote!(CreateUser from User, pick = ["name", "email"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.new_type.to_string(), "CreateUser");
    let pick = input.pick.unwrap();
    assert_eq!(pick, vec!["name", "email"]);
}

#[test]
fn test_parse_schema_type_input_with_rename() {
    let tokens =
        quote::quote!(UserDTO from User, rename = [("id", "user_id"), ("name", "full_name")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.new_type.to_string(), "UserDTO");
    let rename = input.rename.unwrap();
    assert_eq!(rename.len(), 2);
    assert_eq!(rename[0], ("id".to_string(), "user_id".to_string()));
    assert_eq!(rename[1], ("name".to_string(), "full_name".to_string()));
}

#[test]
fn test_parse_schema_type_input_with_single_rename() {
    let tokens = quote::quote!(UserDTO from User, rename = [("id", "user_id")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let rename = input.rename.unwrap();
    assert_eq!(rename.len(), 1);
    assert_eq!(rename[0], ("id".to_string(), "user_id".to_string()));
}

#[test]
fn test_parse_schema_type_input_with_relation_adapters() {
    let tokens = quote::quote!(MemoResponse from Memo, relation_adapters = [("user", UserInMemo), ("category", CategoryInMemo)]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();

    assert_eq!(input.relation_adapters.len(), 2);
    assert_eq!(input.relation_adapters[0].0, "user");
    assert_eq!(input.relation_adapters[0].1.to_string(), "UserInMemo");
    assert_eq!(input.relation_adapters[1].0, "category");
    assert_eq!(input.relation_adapters[1].1.to_string(), "CategoryInMemo");
}

#[test]
fn test_parse_schema_type_input_with_pick_and_rename() {
    let tokens =
        quote::quote!(UserDTO from User, pick = ["id", "name"], rename = [("id", "user_id")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
    assert_eq!(
        input.rename.unwrap(),
        vec![("id".to_string(), "user_id".to_string())]
    );
}

#[test]
fn test_parse_schema_type_input_with_omit_and_rename() {
    let tokens =
        quote::quote!(UserPublic from User, omit = ["password"], rename = [("id", "user_id")]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.omit.unwrap(), vec!["password"]);
    assert_eq!(
        input.rename.unwrap(),
        vec![("id".to_string(), "user_id".to_string())]
    );
}

#[test]
fn test_parse_schema_type_input_with_clone_false() {
    let tokens = quote::quote!(NonCloneUser from User, clone = false);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(!input.derive_clone);
}

#[test]
fn test_parse_schema_type_input_unknown_param_error() {
    let tokens = quote::quote!(UserDTO from User, unknown = ["a"]);
    let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
    assert!(result.is_err());
    match result {
        Err(e) => assert!(e.to_string().contains("unknown parameter")),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_parse_schema_type_input_with_add_single() {
    let tokens = quote::quote!(UserWithTimestamp from User, add = [("created_at": String)]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.new_type.to_string(), "UserWithTimestamp");
    let add = input.add.unwrap();
    assert_eq!(add.len(), 1);
    assert_eq!(add[0].0, "created_at");
}

#[test]
fn test_parse_schema_type_input_with_add_multiple() {
    let tokens = quote::quote!(UserWithMeta from User, add = [("created_at": String), ("updated_at": Option<String>)]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let add = input.add.unwrap();
    assert_eq!(add.len(), 2);
    assert_eq!(add[0].0, "created_at");
    assert_eq!(add[1].0, "updated_at");
}

#[test]
fn test_parse_schema_type_input_with_pick_and_add() {
    let tokens = quote::quote!(CreateUserWithMeta from User, pick = ["name", "email"], add = [("request_id": String)]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
    let add = input.add.unwrap();
    assert_eq!(add.len(), 1);
    assert_eq!(add[0].0, "request_id");
}

#[test]
fn test_parse_schema_type_input_with_omit_and_add() {
    let tokens = quote::quote!(UserPublicWithMeta from User, omit = ["password"], add = [("display_name": String)]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.omit.unwrap(), vec!["password"]);
    let add = input.add.unwrap();
    assert_eq!(add.len(), 1);
    assert_eq!(add[0].0, "display_name");
}

#[test]
fn test_parse_schema_type_input_with_add_complex_type() {
    let tokens = quote::quote!(UserWithVec from User, add = [("tags": Vec<String>)]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let add = input.add.unwrap();
    assert_eq!(add.len(), 1);
    assert_eq!(add[0].0, "tags");
}

#[test]
fn test_parse_schema_type_input_with_partial_all() {
    let tokens = quote::quote!(UpdateUser from User, partial);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(matches!(input.partial, Some(PartialMode::All)));
}

#[test]
fn test_parse_schema_type_input_with_partial_fields() {
    let tokens = quote::quote!(UpdateUser from User, partial = ["name", "email"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    match input.partial {
        Some(PartialMode::Fields(fields)) => {
            assert_eq!(fields, vec!["name", "email"]);
        }
        _ => panic!("Expected PartialMode::Fields"),
    }
}

/// `r#type` names the field `type`: the source names of `rename` / `partial`
/// are kept unraw (so the field lookup and the span lookup both find them),
/// while the `rename` target keeps its spelling.
#[test]
fn test_parse_schema_type_input_strips_raw_prefix_from_source_names() {
    let tokens = quote::quote!(
        UpdateUser from User,
        rename = [("r#type", "r#kind")],
        partial = ["r#type"]
    );
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(
        input.rename.unwrap(),
        vec![("type".to_string(), "r#kind".to_string())]
    );
    match input.partial {
        Some(PartialMode::Fields(fields)) => assert_eq!(fields, vec!["type"]),
        _ => panic!("Expected PartialMode::Fields"),
    }
    assert!(input.spans.field("partial", "type").is_some());
    assert!(input.spans.field("rename", "type").is_some());
}

/// SM3-L1 (round 40): a raw `pick` name is compared unraw (`r#id` is `id`), so
/// its span is found by that spelling, not only by the written one.
#[test]
fn test_parse_schema_type_input_pick_span_of_a_raw_name_is_unraw() {
    let tokens = quote::quote!(UpdateUser from User, pick = ["r#id", "name"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
    assert!(input.spans.field("pick", "id").is_some());
    assert!(input.spans.field("pick", "r#id").is_none());
}

#[test]
fn test_parse_schema_type_input_with_pick_and_partial() {
    let tokens = quote::quote!(UpdateUser from User, pick = ["name", "email"], partial);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
    assert!(matches!(input.partial, Some(PartialMode::All)));
}

#[test]
fn test_parse_schema_type_input_with_pick_and_partial_fields() {
    let tokens = quote::quote!(UpdateUser from User, pick = ["name", "email"], partial = ["name"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
    match input.partial {
        Some(PartialMode::Fields(fields)) => {
            assert_eq!(fields, vec!["name"]);
        }
        _ => panic!("Expected PartialMode::Fields"),
    }
}

#[test]
fn test_parse_schema_type_input_with_ignore() {
    let tokens = quote::quote!(NewType from User, ignore);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(input.ignore_schema);
}

#[test]
fn test_parse_schema_type_input_with_name() {
    let tokens = quote::quote!(NewType from User, name = "CustomName");
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.schema_name.as_deref(), Some("CustomName"));
}

#[test]
fn test_parse_schema_type_input_with_name_and_ignore() {
    let tokens = quote::quote!(NewType from User, name = "CustomName", ignore);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.schema_name.as_deref(), Some("CustomName"));
    assert!(input.ignore_schema);
}

/// L4 (round 17): an unknown rule is an error (in `multipart` mode
/// nothing else rejected it).
#[test]
fn test_parse_schema_type_input_rejects_unknown_rename_all() {
    let tokens = quote::quote!(NewType from User, multipart, rename_all = "camelcase");
    let err = syn::parse2::<SchemaTypeInput>(tokens).err().unwrap();
    assert_eq!(
        err.to_string(),
        "unknown rename rule `rename_all = \"camelcase\"`, expected one of \"lowercase\", \"UPPERCASE\", \"PascalCase\", \"camelCase\", \"snake_case\", \"SCREAMING_SNAKE_CASE\", \"kebab-case\", \"SCREAMING-KEBAB-CASE\""
    );
}

#[test]
fn test_parse_schema_type_input_with_rename_all() {
    let tokens = quote::quote!(NewType from User, rename_all = "snake_case");
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
}

#[test]
fn test_parse_schema_type_input_rename_all_with_other_params() {
    let tokens = quote::quote!(NewType from User, pick = ["id", "name"], rename_all = "snake_case");
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
    assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
}

#[test]
fn test_parse_schema_type_multiple_commas_trailing() {
    let tokens = quote::quote!(NewType from User, pick = ["id"],);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.pick.unwrap(), vec!["id"]);
}

#[test]
fn test_parse_schema_type_all_parameters() {
    let tokens = quote::quote!(
        NewType from crate::models::User,
        pick = ["id", "name"],
        rename = [("id", "user_id")],
        relation_adapters = [("owner", OwnerSummary)],
        add = [("request_id": String)],
        clone = false,
        partial,
        ignore,
        name = "CustomName",
        rename_all = "snake_case",
        multipart,
        omit_default
    );
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    let source_type = &input.source_type;
    assert_eq!(input.new_type, "NewType");
    assert_eq!(
        quote::quote!(#source_type).to_string(),
        "crate :: models :: User"
    );
    assert_eq!(
        input.pick.as_deref(),
        Some(["id".to_string(), "name".to_string()].as_slice())
    );
    assert_eq!(
        input.rename.as_deref(),
        Some([("id".to_string(), "user_id".to_string())].as_slice())
    );
    assert_eq!(input.relation_adapters[0].0, "owner");
    assert_eq!(input.relation_adapters[0].1, "OwnerSummary");
    assert_eq!(input.add.as_ref().unwrap()[0].0, "request_id");
    assert!(!input.derive_clone);
    assert!(matches!(input.partial, Some(PartialMode::All)));
    assert!(input.ignore_schema);
    assert_eq!(input.schema_name.as_deref(), Some("CustomName"));
    assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
    assert!(input.multipart);
    assert!(input.omit_default);
}

// Line 164: Error when "from" keyword is wrong
#[test]
fn test_parse_schema_type_input_wrong_from_keyword() {
    let tokens = quote::quote!(NewType xyz User);
    let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
    assert!(result.is_err());
    match result {
        Err(e) => assert!(e.to_string().contains("expected `from`"), "Error: {e}"),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_parse_schema_type_input_misspelled_from() {
    let tokens = quote::quote!(NewType fron User);
    let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
    assert!(result.is_err());
    match result {
        Err(e) => assert!(
            e.to_string()
                .contains("expected `from` keyword, found `fron`"),
            "Error: {e}"
        ),
        Ok(_) => panic!("Expected error"),
    }
}

// Line 263: Error when both omit and pick are used
#[test]
fn test_parse_schema_type_input_omit_and_pick_error_schema_type() {
    let tokens = quote::quote!(NewType from User, omit = ["a"], pick = ["b"]);
    let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
    assert!(result.is_err());
    match result {
        Err(e) => assert!(
            e.to_string().contains("cannot use both `omit` and `pick`"),
            "Error: {e}"
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_parse_schema_type_input_pick_then_omit_error() {
    // Test the reverse order to ensure both orderings trigger the error
    let tokens = quote::quote!(NewType from User, pick = ["a"], omit = ["b"]);
    let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
    assert!(result.is_err());
    match result {
        Err(e) => assert!(
            e.to_string().contains("cannot use both `omit` and `pick`"),
            "Error: {e}"
        ),
        Ok(_) => panic!("Expected error"),
    }
}

#[test]
fn test_parse_schema_type_input_with_multipart() {
    let tokens = quote::quote!(UploadReq from CreateUploadRequest, multipart);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert_eq!(input.new_type.to_string(), "UploadReq");
    assert!(input.multipart);
}

#[test]
fn test_parse_schema_type_input_with_multipart_and_pick() {
    let tokens =
        quote::quote!(UploadReq from CreateUploadRequest, multipart, pick = ["name", "file"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(input.multipart);
    assert_eq!(input.pick.unwrap(), vec!["name", "file"]);
}

#[test]
fn test_parse_schema_type_input_with_multipart_and_partial() {
    let tokens = quote::quote!(PatchUpload from CreateUploadRequest, multipart, partial);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(input.multipart);
    assert!(matches!(input.partial, Some(PartialMode::All)));
}

#[test]
fn test_parse_schema_type_input_with_omit_default() {
    let tokens = quote::quote!(CreateUser from Model, omit_default);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(input.omit_default);
}

#[test]
fn test_parse_schema_type_input_with_omit_default_and_omit() {
    let tokens = quote::quote!(CreateUser from Model, omit_default, omit = ["password"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(input.omit_default);
    assert_eq!(input.omit.unwrap(), vec!["password"]);
}

#[test]
fn test_parse_schema_type_input_with_omit_default_and_pick() {
    let tokens = quote::quote!(CreateUser from Model, omit_default, pick = ["name", "email"]);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(input.omit_default);
    assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
}

#[test]
fn test_parse_schema_type_input_omit_default_defaults_to_false() {
    let tokens = quote::quote!(CreateUser from User);
    let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
    assert!(!input.omit_default);
}

#[rstest::rstest]
#[case::omit(
    quote::quote!(User, omit = ["a"], omit = ["b"]),
    "duplicate parameter `omit` in schema! invocation"
)]
#[case::pick(
    quote::quote!(User, pick = ["a"], pick = ["b"]),
    "duplicate parameter `pick` in schema! invocation"
)]
fn schema_input_rejects_duplicate_filter(
    #[case] tokens: proc_macro2::TokenStream,
    #[case] expected: &str,
) {
    let Err(error) = syn::parse2::<SchemaInput>(tokens) else {
        panic!("duplicate must fail");
    };
    assert_eq!(error.to_string(), expected);
}
