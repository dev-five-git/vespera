use super::*;

#[test]
fn build_struct_metadata_omits_identity_without_call_site_file() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct User { id: i32 }
    };

    let metadata = build_struct_metadata(&input, "UserSchema".to_string(), None);

    assert_eq!(metadata.name, "UserSchema");
    assert!(metadata.definition.contains("struct User"));
    assert_eq!(metadata.source_identity, None);
}

#[test]
fn build_struct_metadata_attaches_identity_from_call_site_file() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct User { id: i32 }
    };
    let call_site_file = Path::new("some/path.rs");

    let metadata = build_struct_metadata(&input, "UserSchema".to_string(), Some(call_site_file));

    assert_eq!(metadata.name, "UserSchema");
    let identity = metadata.source_identity.expect("identity");
    assert!(
        identity.starts_with("some/path.rs::User@"),
        "identity carries the ident position: {identity}"
    );
}

/// Same-named structs in two inline `mod`s of ONE file get distinct
/// identities (span position), so the second registration is rejected as a
/// duplicate instead of silently replacing the first.
#[test]
fn same_named_structs_in_inline_mods_of_one_file_conflict() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("models.rs");
    let source =
        "mod a {\n    struct User { id: i32 }\n}\nmod b {\n    struct User { name: String }\n}\n";
    std::fs::write(&file, source).unwrap();
    let parsed: syn::File = syn::parse_str(source).unwrap();
    let structs: Vec<syn::DeriveInput> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) => module.content.as_ref().map(|(_, items)| &items[0]),
            _ => None,
        })
        .map(|item| syn::parse2(quote::quote!(#item)).unwrap())
        .collect();
    let key = "__test_inline_mod_duplicate__".to_string();
    remove_current_crate_schema(&key);

    let first = build_struct_metadata(&structs[0], key.clone(), Some(&file));
    let second = build_struct_metadata(&structs[1], key.clone(), Some(&file));
    assert_ne!(first.source_identity, second.source_identity);
    assert!(register_schema(key.clone(), first).is_ok());
    assert!(
        register_schema(key.clone(), second).is_err(),
        "two different `User` structs in one file must conflict"
    );
    remove_current_crate_schema(&key);
}

/// A single item whose position changed (IDE edit) is still a replacement;
/// when the file holds zero (not locatable) or several items of that name,
/// the move is ambiguous and the strict `line:column` identity applies.
#[rstest::rstest]
#[case::single_item("\n\nstruct User { id: i64 }\n", true)]
#[case::not_found("\n\nstruct Other { id: i64 }\n", false)]
#[case::second_in_fn_body("fn f() { struct User; }\nstruct User { id: i64 }\n", false)]
fn moved_item_replacement(#[case] source: &str, #[case] replaced: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("models.rs");
    std::fs::write(&file, source).unwrap();
    let path = file.display();
    let key = format!("__test_moved_item_{replaced}_{}__", source.len());
    remove_current_crate_schema(&key);

    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct User { id: i32 }".to_string())
                .with_source_identity(format!("{path}::User@1:7")),
        )
        .is_ok()
    );
    let second = register_schema(
        key.clone(),
        StructMetadata::new(key.clone(), "struct User { id: i64 }".to_string())
            .with_source_identity(format!("{path}::User@3:7")),
    );
    assert_eq!(second.is_ok(), replaced);
    assert_eq!(
        current_crate_schemas()[&key].definition.contains("i64"),
        replaced
    );
    remove_current_crate_schema(&key);
}

#[rstest::rstest]
#[case::nested_mods("mod a { struct X; mod b { enum X {} } } struct X;", 3)]
#[case::other_names("struct Y; mod a { struct Z; }", 0)]
#[case::external_mod("mod a; struct X;", 1)]
#[case::fn_body("fn f() { struct X; } struct X;", 2)]
#[case::impl_and_const("impl T { fn g() { enum X {} } } const C: () = { struct X; };", 2)]
#[case::static_and_nested_block("static S: u8 = { if true { struct X; } 0 };", 1)]
fn count_items_named_descends_into_nested_items(#[case] source: &str, #[case] expected: usize) {
    let parsed: syn::File = syn::parse_str(source).unwrap();
    assert_eq!(count_items_named(&parsed.items, "X"), expected);
}

#[test]
fn test_process_derive_schema_struct() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct User {
            name: String,
            age: u32,
        }
    };
    let (metadata, _expanded) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "User");
    assert!(metadata.definition.contains("struct User"));
}

#[test]
fn test_process_derive_schema_enum() {
    let input: syn::DeriveInput = syn::parse_quote! {
        enum Status {
            Active,
            Inactive,
        }
    };
    let (metadata, _expanded) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "Status");
    assert!(metadata.definition.contains("enum Status"));
}

#[test]
fn test_process_derive_schema_generic() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Container<T> {
            value: T,
        }
    };
    let (metadata, _expanded) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "Container");
}

#[test]
fn test_extract_schema_name_attr_with_name() {
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[schema(name = "CustomName")]
    };
    let result = extract_schema_name_attr(&attrs);
    assert_eq!(result, Some("CustomName".to_string()));
}

#[test]
fn explicit_schema_name_is_retained_in_metadata() {
    let input: syn::DeriveInput = syn::parse_quote! {
        #[schema(name = "SharedThing")]
        struct Item { id: i32 }
    };

    let (metadata, _) = process_derive_schema(&input);

    let metadata = metadata.expect("derive metadata");
    assert_eq!(metadata.name, "SharedThing");
    assert!(metadata.explicit_schema_name);
}

#[test]
fn test_extract_schema_name_attr_without_name() {
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[derive(Debug)]
    };
    let result = extract_schema_name_attr(&attrs);
    assert_eq!(result, None);
}

#[test]
fn test_extract_schema_name_attr_empty_schema() {
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[schema]
    };
    let result = extract_schema_name_attr(&attrs);
    assert_eq!(result, None);
}

#[test]
fn test_extract_schema_name_attr_with_other_attrs() {
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[derive(Clone)]
        #[schema(name = "MySchema")]
        #[serde(rename_all = "camelCase")]
    };
    let result = extract_schema_name_attr(&attrs);
    assert_eq!(result, Some("MySchema".to_string()));
}

#[test]
fn test_process_derive_schema_simple() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct User {
            id: i32,
            name: String,
        }
    };
    let (metadata, _tokens) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "User");
    assert!(metadata.definition.contains("User"));
}

#[test]
fn test_process_derive_schema_with_custom_name() {
    let input: syn::DeriveInput = syn::parse_quote! {
        #[schema(name = "CustomUserSchema")]
        struct User {
            id: i32,
        }
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "CustomUserSchema");
}

#[test]
fn test_process_derive_schema_with_generics() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Container<T> {
            value: T,
        }
    };
    let (metadata, _tokens) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "Container");
}

#[test]
fn test_extract_schema_name_attr_non_name_meta_key() {
    // #[schema(other = "foo")] — has schema attr but no "name" key
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[schema(other = "foo")]
    };
    let result = extract_schema_name_attr(&attrs);
    assert_eq!(result, None);
}

#[test]
fn test_extract_defaults_from_file_finds_functions() {
    // Directly tests the extracted function (covers lines 123-131)
    let file_ast: syn::File = syn::parse_quote! {
        fn default_count() -> i32 { 42 }
        fn default_name() -> String { "hello".to_string() }
    };
    let fn_defaults = vec![
        ("count".to_string(), "default_count".to_string()),
        ("name".to_string(), "default_name".to_string()),
    ];
    let result = extract_defaults_from_file(&fn_defaults, &file_ast);
    assert_eq!(result.get("count"), Some(&serde_json::json!(42)));
    assert_eq!(result.get("name"), Some(&serde_json::json!("hello")));
}

#[test]
fn test_extract_defaults_from_file_missing_function() {
    // Function not found in AST -> skipped
    let file_ast: syn::File = syn::parse_quote! {
        fn other_function() -> i32 { 0 }
    };
    let fn_defaults = vec![("count".to_string(), "nonexistent_fn".to_string())];
    let result = extract_defaults_from_file(&fn_defaults, &file_ast);
    assert!(result.is_empty());
}

#[test]
fn test_extract_defaults_from_file_non_extractable_value() {
    // Function exists but returns an assignment statement or block (not directly extractable)
    let file_ast: syn::File = syn::parse_quote! {
        fn default_value() -> String {
            let x = String::new();
            x  // Assignment before return - block statement
        }
    };
    let fn_defaults = vec![("value".to_string(), "default_value".to_string())];
    let result = extract_defaults_from_file(&fn_defaults, &file_ast);
    // Block statements with multiple statements are not extractable
    assert!(result.is_empty());
}

#[test]
fn test_extract_defaults_from_file_empty_input() {
    let file_ast: syn::File = syn::parse_quote! {};
    let fn_defaults: Vec<(String, String)> = vec![];
    let result = extract_defaults_from_file(&fn_defaults, &file_ast);
    assert!(result.is_empty());
}

#[test]
fn test_extract_schema_name_attr_multiple_schema_attrs() {
    // Two #[schema] attrs — first one with name wins
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[schema(name = "First")]
        #[schema(name = "Second")]
    };
    let result = extract_schema_name_attr(&attrs);
    assert_eq!(result, Some("First".to_string()));
}

#[test]
fn test_extract_schema_name_attr_schema_with_unknown_key_value() {
    // `#[schema(other = "x", name = "MyName")]` — the unknown `other`
    // key's value is now consumed so `parse_nested_meta` reaches `name`
    // instead of bailing early; the custom name is no longer lost.
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[schema(other = "x", name = "MyName")]
    };
    assert_eq!(
        extract_schema_name_attr(&attrs),
        Some("MyName".to_string()),
        "a `name` after an unknown key must still be extracted"
    );
}

#[test]
fn test_extract_schema_name_attr_name_before_unknown() {
    // name comes FIRST, so it's extracted before the unknown key causes a bail
    let attrs: Vec<syn::Attribute> = syn::parse_quote! {
        #[schema(name = "Found", other = "x")]
    };
    let result = extract_schema_name_attr(&attrs);
    // name is parsed successfully; parse_nested_meta may error on `other` but name is already set
    assert_eq!(result, Some("Found".to_string()));
}

// ========== Coverage: process_derive_schema struct variants ==========

#[test]
fn test_process_derive_schema_unit_struct() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Unit;
    };
    let (metadata, tokens) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "Unit");
    assert!(metadata.definition.contains("Unit"));
    assert!(
        tokens
            .to_string()
            .contains("impl :: vespera :: Schema for Unit"),
        "unit structs should emit the Schema marker impl: {tokens}"
    );
}

#[test]
fn test_process_derive_schema_tuple_struct() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Pair(i32, String);
    };
    let (metadata, tokens) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "Pair");
    assert!(metadata.definition.contains("Pair"));
    assert!(
        tokens
            .to_string()
            .contains("impl :: vespera :: Schema for Pair"),
        "tuple structs should emit the Schema marker impl: {tokens}"
    );
}

#[test]
fn test_process_derive_schema_empty_struct() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Empty {}
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "Empty");
}

#[test]
fn test_process_derive_schema_with_lifetime() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Ref<'a> {
            data: &'a str,
        }
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "Ref");
}

#[test]
fn test_process_derive_schema_with_serde_attrs() {
    let input: syn::DeriveInput = syn::parse_quote! {
        #[serde(rename_all = "camelCase")]
        struct UserResponse {
            user_name: String,
            #[serde(skip)]
            internal_id: u64,
        }
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "UserResponse");
    assert!(metadata.definition.contains("camelCase"));
    assert!(metadata.definition.contains("skip"));
}

// ========== Coverage: metadata field verification ==========

#[test]
fn test_process_derive_schema_include_in_openapi_true() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Visible { x: i32 }
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert!(
        metadata.include_in_openapi,
        "Schema-derived types must have include_in_openapi=true"
    );
}

#[test]
fn test_process_derive_schema_definition_contains_fields() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct WithFields {
            id: u64,
            name: String,
            active: bool,
        }
    };
    let (metadata, _) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert!(metadata.definition.contains("id"));
    assert!(metadata.definition.contains("u64"));
    assert!(metadata.definition.contains("name"));
    assert!(metadata.definition.contains("active"));
    assert!(metadata.definition.contains("bool"));
}

// ========== Coverage: SCHEMA_STORAGE direct usage ==========

/// Remove a schema entry from the current crate's bucket (test cleanup).
fn remove_current_crate_schema(key: &str) {
    let mut guard = SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(bucket) = guard.get_mut(&current_crate_key()) {
        Arc::make_mut(bucket).remove(key);
    }
}

#[test]
fn test_schema_storage_insert_and_get() {
    let key = "__test_coverage_type__".to_string();
    remove_current_crate_schema(&key);

    insert_schema(
        key.clone(),
        StructMetadata::new(key.clone(), "struct __test_coverage_type__ {}".to_string()),
    );

    let schemas = current_crate_schemas();
    let meta = schemas.get(&key);
    assert!(meta.is_some(), "Inserted metadata should be retrievable");
    let meta = meta.unwrap();
    assert_eq!(meta.name, key);
    assert!(meta.include_in_openapi);

    remove_current_crate_schema(&key);
}

#[test]
fn test_schema_storage_overwrite() {
    let key = "__test_overwrite_type__".to_string();
    remove_current_crate_schema(&key);
    insert_schema(
        key.clone(),
        StructMetadata::new(key.clone(), "struct V1 {}".to_string()),
    );
    insert_schema(
        key.clone(),
        StructMetadata::new(key.clone(), "struct V2 {}".to_string()),
    );
    let schemas = current_crate_schemas();
    let meta = schemas.get(&key).unwrap();
    assert!(meta.definition.contains("V2"), "Last insert should win");
    remove_current_crate_schema(&key);
}

#[test]
fn test_register_schema_rejects_conflicting_definition() {
    let key = "__test_conflict_type__".to_string();
    remove_current_crate_schema(&key);
    // First registration wins.
    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct A { x: i32 }".to_string()),
        )
        .is_ok()
    );
    // Identical re-registration is idempotent.
    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct A { x: i32 }".to_string()),
        )
        .is_ok()
    );
    // A DIFFERENT definition under the same name is rejected.
    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct A { y: u64 }".to_string()),
        )
        .is_err()
    );
    remove_current_crate_schema(&key);
}

#[test]
fn test_register_schema_replaces_same_source_identity() {
    let key = "__test_same_source_replacement__".to_string();
    remove_current_crate_schema(&key);
    let source_identity = "src/models/user.rs::User".to_string();

    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct User { id: i32 }".to_string())
                .with_source_identity(source_identity.clone()),
        )
        .is_ok()
    );
    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct User { id: i64 }".to_string())
                .with_source_identity(source_identity),
        )
        .is_ok()
    );

    let schemas = current_crate_schemas();
    let meta = schemas.get(&key).expect("schema should remain registered");
    assert!(meta.definition.contains("i64"));
    remove_current_crate_schema(&key);
}

#[test]
fn test_register_schema_rejects_different_source_identity() {
    let key = "__test_distinct_source_conflict__".to_string();
    remove_current_crate_schema(&key);

    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct UserA { id: i32 }".to_string())
                .with_source_identity("src/a.rs::User".to_string()),
        )
        .is_ok()
    );
    assert!(
        register_schema(
            key.clone(),
            StructMetadata::new(key.clone(), "struct UserB { id: i32 }".to_string())
                .with_source_identity("src/b.rs::User".to_string()),
        )
        .is_err()
    );
    remove_current_crate_schema(&key);
}

#[test]
fn test_invalid_derive_schema_does_not_register_or_poison_storage() {
    let key = "__InvalidConstraintDoesNotPoison".to_string();
    remove_current_crate_schema(&key);
    let invalid: syn::DeriveInput = syn::parse_quote! {
        struct __InvalidConstraintDoesNotPoison {
            #[schema(min_length = "bad")]
            name: String,
        }
    };

    let (metadata, tokens) = process_derive_schema(&invalid);
    assert!(
        metadata.is_none(),
        "invalid constraints must skip registration"
    );
    assert!(tokens.to_string().contains("compile_error"));

    let valid: syn::DeriveInput = syn::parse_quote! {
        struct __InvalidConstraintDoesNotPoison {
            name: String,
        }
    };
    let (metadata, tokens) = process_derive_schema(&valid);
    assert!(
        tokens
            .to_string()
            .contains("impl :: vespera :: Schema for __InvalidConstraintDoesNotPoison"),
        "valid re-registration should emit the Schema marker impl: {tokens}"
    );
    let metadata = metadata.expect("valid schema metadata");
    assert!(register_schema(key.clone(), metadata).is_ok());
    remove_current_crate_schema(&key);
}

#[test]
fn test_schema_storage_crate_scoping_isolation() {
    // A schema registered under a DIFFERENT crate's bucket must never leak
    // into the current crate's snapshot — the cross-crate contamination
    // fix for long-lived rust-analyzer proc-macro servers.
    let fake_crate = "__fake_other_crate_dir__".to_string();
    let key = "__isolated_schema__".to_string();
    {
        let mut guard = SCHEMA_STORAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::make_mut(
            guard
                .entry(fake_crate.clone())
                .or_insert_with(|| Arc::new(HashMap::new())),
        )
        .insert(
            key.clone(),
            StructMetadata::new(key.clone(), "struct Isolated {}".to_string()),
        );
    }
    let mine = current_crate_schemas();
    assert!(
        !mine.contains_key(&key),
        "another crate's schema must not leak into this crate's snapshot"
    );
    SCHEMA_STORAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&fake_crate);
}

#[test]
fn test_extract_field_defaults_from_path_with_default_fn() {
    // Exercises lines 125-133 (was 118-119, 123-124 before refactor):
    // get_parsed_file succeeds and extract_defaults_from_file runs.
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("defaults.rs");
    std::fs::write(
        &file_path,
        r#"
fn default_status() -> String {
"active".to_string()
}

struct Config {
#[serde(default = "default_status")]
status: String,
}
"#,
    )
    .unwrap();

    let input: syn::DeriveInput = syn::parse_quote! {
        struct Config {
            #[serde(default = "default_status")]
            status: String,
        }
    };

    let defaults = extract_field_defaults_from_path(&input, &file_path);
    // The function should find default_status and extract its return value
    assert!(
        defaults.contains_key("status"),
        "Should extract default for 'status' field"
    );
}

#[test]
fn test_extract_field_defaults_from_path_file_not_found() {
    // Exercises the else branch: get_parsed_file returns None for non-existent file
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Config {
            #[serde(default = "default_val")]
            value: String,
        }
    };

    let defaults = extract_field_defaults_from_path(&input, Path::new("/nonexistent/path/foo.rs"));
    assert!(
        defaults.is_empty(),
        "Should return empty defaults when file not found"
    );
}

#[test]
fn test_extract_field_defaults_from_path_no_fn_defaults() {
    // Exercises the early return: fn_defaults is empty
    let temp_dir = tempfile::TempDir::new().unwrap();
    let file_path = temp_dir.path().join("simple.rs");
    std::fs::write(&file_path, "struct Foo { x: i32 }").unwrap();

    let input: syn::DeriveInput = syn::parse_quote! {
        struct Foo {
            x: i32,
        }
    };

    let defaults = extract_field_defaults_from_path(&input, &file_path);
    assert!(defaults.is_empty(), "No serde defaults -> empty result");
}

#[test]
fn test_extract_field_defaults_from_path_tuple_struct() {
    // Exercises line 101: Fields::Named else branch (tuple struct has unnamed fields)
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Pair(String, i32);
    };
    let defaults = extract_field_defaults_from_path(&input, Path::new("/dummy.rs"));
    assert!(
        defaults.is_empty(),
        "Tuple struct should return empty defaults"
    );
}

#[test]
fn test_extract_field_defaults_from_path_enum() {
    // Exercises line 103: Data::Struct else branch (enum)
    let input: syn::DeriveInput = syn::parse_quote! {
        enum Status { Active, Inactive }
    };
    let defaults = extract_field_defaults_from_path(&input, Path::new("/dummy.rs"));
    assert!(defaults.is_empty(), "Enum should return empty defaults");
}

#[test]
fn test_process_derive_schema_ref_override_excludes_openapi() {
    let input: syn::DeriveInput = syn::parse_quote! {
        #[derive(Clone)]
        #[schema(ref = "ExternalUser")]
        struct UserSchema {
            id: i32,
        }
    };

    let (metadata, tokens) = process_derive_schema(&input);
    let metadata = metadata.expect("valid schema metadata");
    assert_eq!(metadata.name, "UserSchema");
    assert!(!metadata.include_in_openapi);
    assert!(
        tokens
            .to_string()
            .contains("impl :: vespera :: Schema for UserSchema"),
        "ref overrides should still emit the Schema marker impl: {tokens}"
    );
}

#[test]
fn schema_attribute_summary_stops_after_name_and_ref_are_found() {
    let input: syn::DeriveInput = syn::parse_quote! {
        #[schema(name = "ExternalUser", ref = "components.schemas.User")]
        #[schema(name = "Ignored")]
        struct User { id: i32 }
    };

    let summary = collect_schema_attribute_summary(&input.attrs).expect("valid attrs");

    assert_eq!(summary.name.as_deref(), Some("ExternalUser"));
    assert!(summary.has_ref_override);
}

/// Malformed container `#[schema(...)]` attributes are compile errors
/// instead of being silently ignored.
#[rstest::rstest]
#[case::name_not_string("name_not_string", quote::quote!(#[schema(name = Foo)]))]
#[case::ref_not_string("ref_not_string", quote::quote!(#[schema(ref = 42)]))]
#[case::name_missing_value("name_missing_value", quote::quote!(#[schema(name)]))]
#[case::garbage("garbage", quote::quote!(#[schema(name = "A" "B")]))]
fn malformed_container_schema_attr_is_compile_error(
    #[case] case: &str,
    #[case] attr: proc_macro2::TokenStream,
) {
    let input: syn::DeriveInput = syn::parse_quote! {
        #attr
        struct User { id: i32 }
    };
    let (metadata, tokens) = process_derive_schema(&input);
    assert!(metadata.is_none(), "{case}: must not register");
    insta::assert_snapshot!(
        format!("malformed_container_schema_attr_{case}"),
        tokens.to_string()
    );
}

#[rstest::rstest]
#[case::unknown_key_value(quote::quote!(#[schema(other = "x", name = "N")]), Some("N"))]
#[case::unknown_key_list(quote::quote!(#[schema(extra(a = 1), name = "M")]), Some("M"))]
#[case::bare(quote::quote!(#[schema]), None)]
fn container_schema_attr_tolerates_unknown_keys(
    #[case] attr: proc_macro2::TokenStream,
    #[case] expected: Option<&str>,
) {
    let input: syn::DeriveInput = syn::parse_quote! {
        #attr
        struct User { id: i32 }
    };
    let summary = collect_schema_attribute_summary(&input.attrs).expect("tolerated");
    assert_eq!(summary.name.as_deref(), expected);
}

/// `#[serde(tag = "...")]` newtype variants wrapping a non-object value are
/// rejected at derive time; struct / map inner values are accepted.
#[rstest::rstest]
#[case::string_inner("string_inner", quote::quote!(Text(String)), true)]
#[case::vec_inner("vec_inner", quote::quote!(List(Vec<i32>)), true)]
#[case::tuple_inner("tuple_inner", quote::quote!(Pair((i32, i32))), true)]
#[case::struct_inner("struct_inner", quote::quote!(Detail(Payload)), false)]
#[case::map_inner("map_inner", quote::quote!(Meta(std::collections::HashMap<String, i32>)), false)]
#[case::skipped_variant("skipped_variant", quote::quote!(#[serde(skip)] Text(String)), false)]
fn internally_tagged_newtype_validation(
    #[case] case: &str,
    #[case] variant: proc_macro2::TokenStream,
    #[case] is_error: bool,
) {
    let input: syn::DeriveInput = syn::parse_quote! {
        #[serde(tag = "type")]
        enum Event { Ping, #variant }
    };
    let (metadata, tokens) = process_derive_schema(&input);
    assert_eq!(metadata.is_none(), is_error, "{case}");
    if is_error {
        insta::assert_snapshot!(
            format!("internally_tagged_newtype_error_{case}"),
            tokens.to_string()
        );
    }
}

#[test]
fn process_derive_schema_returns_all_unresolved_serde_default_errors() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct InvalidDefaults {
            #[serde(default = "default_first")]
            first: String,
            #[serde(default = "default_second")]
            second: i32,
        }
    };

    let (metadata, tokens) = process_derive_schema(&input);
    let file = syn::parse2::<syn::File>(tokens).expect("compile errors parse as Rust items");
    let messages: Vec<String> = file
        .items
        .iter()
        .filter_map(|item| {
            let syn::Item::Macro(item_macro) = item else {
                return None;
            };
            // `syn::Error::to_compile_error` emits `::core::compile_error!`, so
            // match the last path segment rather than the whole path.
            let invoked = item_macro.mac.path.segments.last()?;
            if invoked.ident != "compile_error" {
                return None;
            }
            syn::parse2::<syn::LitStr>(item_macro.mac.tokens.clone())
                .ok()
                .map(|literal| literal.value())
        })
        .collect();

    assert!(metadata.is_none());
    assert_eq!(
        messages,
        vec![
            "cannot statically determine the OpenAPI default for field `first` which has `#[serde(default)]`; add an explicit `#[schema(default = \"...\")]`",
            "cannot statically determine the OpenAPI default for field `second` which has `#[serde(default)]`; add an explicit `#[schema(default = \"...\")]`",
        ]
    );
}

#[test]
fn extract_field_defaults_skips_qualified_default_functions() {
    let input: syn::DeriveInput = syn::parse_quote! {
        struct Config {
            #[serde(default = "crate::defaults::name")]
            name: String,
        }
    };

    let defaults = extract_field_defaults_from_path(&input, Path::new("unused.rs"));

    assert!(defaults.is_empty());
}

#[test]
fn serde_default_validation_accepts_schema_function_and_type_defaults() {
    let explicit_schema: syn::DeriveInput = syn::parse_quote! {
        struct Explicit {
            #[serde(default = "missing")]
            #[schema(default = "fallback")]
            value: CustomType,
        }
    };
    let resolved_function: syn::DeriveInput = syn::parse_quote! {
        struct FunctionDefault {
            #[serde(default = "default_name")]
            name: String,
        }
    };
    let type_default: syn::DeriveInput = syn::parse_quote! {
        struct TypeDefault {
            #[serde(default)]
            active: bool,
        }
    };
    let function_values = BTreeMap::from([("name".to_string(), serde_json::json!("guest"))]);

    assert!(validate_serde_default_values(&explicit_schema, &BTreeMap::new()).is_ok());
    assert!(validate_serde_default_values(&resolved_function, &function_values).is_ok());
    assert!(validate_serde_default_values(&type_default, &BTreeMap::new()).is_ok());
}

#[test]
fn serde_default_resolution_distinguishes_present_missing_and_type_defaults() {
    let function_field: syn::Field = syn::parse_quote! {
        #[serde(default = "default_name")]
        name: String
    };
    let type_field: syn::Field = syn::parse_quote! {
        #[serde(default)]
        count: i32
    };
    let values = BTreeMap::from([("name".to_string(), serde_json::json!("guest"))]);
    let function_name = "default_name".to_string();

    assert!(serde_default_is_resolvable(
        &function_field,
        Some(&function_name),
        &values
    ));
    assert!(!serde_default_is_resolvable(
        &function_field,
        Some(&function_name),
        &BTreeMap::new()
    ));
    assert!(serde_default_is_resolvable(
        &type_field,
        None,
        &BTreeMap::new()
    ));
}

#[test]
fn schema_default_detection_handles_matching_and_nonmatching_attributes() {
    let matching: Vec<syn::Attribute> = syn::parse_quote!(#[schema(default = "fallback")]);
    let other_schema_key: Vec<syn::Attribute> = syn::parse_quote!(#[schema(example = "sample")]);
    let unrelated: Vec<syn::Attribute> = syn::parse_quote!(#[serde(default)]);

    assert!(has_schema_default(&matching));
    assert!(!has_schema_default(&other_schema_key));
    assert!(!has_schema_default(&unrelated));
}

#[test]
fn cached_default_functions_reuses_the_cached_arc_for_unchanged_files() {
    let temp_dir = tempfile::TempDir::new().expect("temporary directory");
    let file_path = temp_dir.path().join("cached_defaults.rs");
    std::fs::write(&file_path, "fn default_count() -> i32 { 42 }")
        .expect("write default function fixture");

    let first = cached_default_functions(&file_path).expect("first parse populates cache");
    let second = cached_default_functions(&file_path).expect("second parse reads cache");

    assert_eq!(first.get("default_count"), Some(&serde_json::json!(42)));
    assert!(Arc::ptr_eq(&first, &second));
}

/// An identity without a `file::Ident` part (never produced by `derive`, e.g.
/// hand-built metadata) can never be proven to be the same item that moved.
#[test]
fn identity_without_file_component_is_never_a_move() {
    assert!(!is_same_item_moved(Some("User@1:7"), Some("User@3:7")));
}
