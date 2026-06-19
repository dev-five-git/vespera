    use rstest::rstest;
    use serial_test::serial;

    use super::*;

    // ── Test support ─────────────────────────────────────────────────────

    /// Render generated item tokens as formatted Rust source so snapshots
    /// review like real code instead of a single token-soup line.
    fn pretty(tokens: &proc_macro2::TokenStream) -> String {
        let file: syn::File =
            syn::parse2(tokens.clone()).expect("generated tokens must parse as Rust items");
        prettyplease::unparse(&file)
    }

    /// Compact [`InlineField`] constructor for table-driven cases.
    fn field(name: &str, ty: proc_macro2::TokenStream, attrs: Vec<syn::Attribute>) -> InlineField {
        InlineField {
            name: syn::Ident::new(name, proc_macro2::Span::call_site()),
            ty,
            attrs,
        }
    }

    /// Compact [`InlineRelationType`] constructor for table-driven cases.
    fn inline(name: &str, rename_all: &str, fields: Vec<InlineField>) -> InlineRelationType {
        InlineRelationType {
            type_name: syn::Ident::new(name, proc_macro2::Span::call_site()),
            fields,
            rename_all: rename_all.to_string(),
        }
    }

    /// Compact [`RelationFieldInfo`] constructor — the original tests
    /// repeated this 10-line struct literal a dozen times.
    fn rel(
        field_name: &str,
        relation_type: &str,
        schema_path: proc_macro2::TokenStream,
    ) -> RelationFieldInfo {
        RelationFieldInfo {
            field_name: syn::Ident::new(field_name, proc_macro2::Span::call_site()),
            relation_type: relation_type.to_string(),
            schema_path,
            is_optional: false,
            inline_type_info: None,
            relation_enum: None,
            fk_column: None,
            via_rel: None,
        }
    }

    /// Sorted field names of a generated inline type — list equality
    /// asserts both inclusions and exclusions in one comparison.
    fn field_names(inline_type: &InlineRelationType) -> Vec<String> {
        let mut names: Vec<String> = inline_type
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        names.sort();
        names
    }

    const MEMO_MODULE: [&str; 3] = ["crate", "models", "memo"];

    fn module_path(segments: &[&str]) -> Vec<String> {
        segments.iter().map(ToString::to_string).collect()
    }

    /// Run `body` with `CARGO_MANIFEST_DIR` pointing at `dir`, restoring
    /// the original value afterwards.
    fn with_manifest_dir<T>(dir: &std::path::Path, body: impl FnOnce() -> T) -> T {
        let original = std::env::var("CARGO_MANIFEST_DIR").ok();
        // SAFETY: callers are #[serial] tests — no concurrent env access.
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", dir) };
        let result = body();
        // SAFETY: same as above.
        unsafe {
            match original {
                Some(value) => std::env::set_var("CARGO_MANIFEST_DIR", value),
                None => std::env::remove_var("CARGO_MANIFEST_DIR"),
            }
        }
        result
    }

    // ── generate_inline_type_definition: snapshot the full output ───────
    //
    // The generated struct IS the contract — snapshotting the whole
    // pretty-printed item locks derives, serde attributes, field types,
    // and rename_all in one reviewable artifact, instead of probing a
    // handful of `contains` substrings around unverified output.

    #[rstest]
    #[case::two_plain_fields_camel_case(
        "two_plain_fields_camel_case",
        inline(
            "UserInline",
            "camelCase",
            vec![field("id", quote!(i32), vec![]), field("name", quote!(String), vec![])],
        )
    )]
    #[case::field_attr_rename_snake_case(
        "field_attr_rename_snake_case",
        inline(
            "TestType",
            "snake_case",
            vec![field(
                "field",
                quote!(String),
                vec![syn::parse_quote!(#[serde(rename = "renamed")])],
            )],
        )
    )]
    #[case::empty_fields("empty_fields", inline("EmptyType", "camelCase", vec![]))]
    #[case::multiple_field_attrs_pascal_case(
        "multiple_field_attrs_pascal_case",
        inline(
            "MultiAttrType",
            "PascalCase",
            vec![field(
                "field",
                quote!(String),
                vec![
                    syn::parse_quote!(#[serde(default)]),
                    syn::parse_quote!(#[serde(skip_serializing_if = "Option::is_none")]),
                ],
            )],
        )
    )]
    #[case::complex_field_types(
        "complex_field_types",
        inline(
            "ComplexType",
            "camelCase",
            vec![
                field("id", quote!(i32), vec![]),
                field("tags", quote!(Vec<String>), vec![]),
                field(
                    "metadata",
                    quote!(Option<std::collections::HashMap<String, serde_json::Value>>),
                    vec![],
                ),
            ],
        )
    )]
    #[case::doc_attribute(
        "doc_attribute",
        inline(
            "DocType",
            "camelCase",
            vec![field(
                "documented_field",
                quote!(String),
                vec![syn::parse_quote!(#[doc = "This is a documented field"])],
            )],
        )
    )]
    fn generate_inline_type_definition_snapshot(
        #[case] snapshot_name: &str,
        #[case] inline_type: InlineRelationType,
    ) {
        // Explicit snapshot name per case: insta's auto-naming counts
        // duplicate assertions per *function* in execution order, which
        // shuffles across parallel rstest cases.
        insta::assert_snapshot!(
            snapshot_name,
            pretty(&generate_inline_type_definition(&inline_type))
        );
    }

    #[test]
    fn inline_field_struct_holds_constructor_inputs() {
        let field = field(
            "test_field",
            quote!(Option<i32>),
            vec![syn::parse_quote!(#[doc = "Test doc"])],
        );
        assert_eq!(field.name.to_string(), "test_field");
        assert!(!field.attrs.is_empty());
    }

    #[test]
    fn inline_relation_type_struct_holds_constructor_inputs() {
        let inline_type = inline("TestRelation", "SCREAMING_SNAKE_CASE", vec![]);
        assert_eq!(inline_type.type_name.to_string(), "TestRelation");
        assert_eq!(inline_type.rename_all, "SCREAMING_SNAKE_CASE");
        assert!(inline_type.fields.is_empty());
    }

    // ── generate_inline_relation_type_from_def ──────────────────────────

    #[test]
    fn from_def_has_many_is_not_circular() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub name: String,
            pub memos: HasMany<memo::Entity>
        }";
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("MemoSchema", proc_macro2::Span::call_site()),
            &rel("user", "BelongsTo", quote!(super::user::Schema)),
            &module_path(&MEMO_MODULE),
            None,
            model_def,
        );
        assert!(result.is_none(), "HasMany back-references are not circular");
    }

    #[test]
    fn from_def_belongs_to_is_circular_and_strips_the_relation() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub name: String,
            pub memo: BelongsTo<memo::Entity>
        }";
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("MemoSchema", proc_macro2::Span::call_site()),
            &rel("user", "BelongsTo", quote!(super::user::Schema)),
            &module_path(&MEMO_MODULE),
            None,
            model_def,
        )
        .expect("BelongsTo back-reference is circular");

        assert_eq!(result.type_name.to_string(), "MemoSchema_User");
        assert_eq!(field_names(&result), ["id", "name"]);
    }

    #[test]
    fn from_def_no_circular_reference_returns_none() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub name: String
        }";
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("TestSchema", proc_macro2::Span::call_site()),
            &rel("other", "BelongsTo", quote!(super::other::Schema)),
            &module_path(&["crate", "models", "test"]),
            None,
            model_def,
        );
        assert!(result.is_none(), "no circular fields means no inline type");
    }

    #[test]
    fn from_def_schema_name_override_names_the_inline_type() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub memo: BelongsTo<memo::Entity>
        }";
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("Schema", proc_macro2::Span::call_site()),
            &rel("user", "BelongsTo", quote!(super::user::Schema)),
            &module_path(&MEMO_MODULE),
            Some("MemoSchema"),
            model_def,
        )
        .expect("circular reference present");
        assert_eq!(result.type_name.to_string(), "MemoSchema_User");
    }

    #[test]
    fn from_def_invalid_model_source_returns_none() {
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("TestSchema", proc_macro2::Span::call_site()),
            &rel("user", "BelongsTo", quote!(super::user::Schema)),
            &module_path(&["crate"]),
            None,
            "invalid rust code",
        );
        assert!(result.is_none());
    }

    #[test]
    fn from_def_skips_every_relation_typed_field() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub name: String,
            pub memo: BelongsTo<memo::Entity>,
            pub posts: HasMany<post::Entity>,
            pub profile: HasOne<profile::Entity>
        }";
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("MemoSchema", proc_macro2::Span::call_site()),
            &rel("user", "BelongsTo", quote!(super::user::Schema)),
            &module_path(&MEMO_MODULE),
            None,
            model_def,
        )
        .expect("circular reference present");
        assert_eq!(
            field_names(&result),
            ["id", "name"],
            "circular AND non-circular relation fields must all be stripped"
        );
    }

    #[test]
    fn from_def_skips_serde_skip_fields() {
        let model_def = r"pub struct Model {
            pub id: i32,
            #[serde(skip)]
            pub internal_cache: String,
            pub name: String,
            pub memo: BelongsTo<memo::Entity>
        }";
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("MemoSchema", proc_macro2::Span::call_site()),
            &rel("user", "BelongsTo", quote!(super::user::Schema)),
            &module_path(&MEMO_MODULE),
            None,
            model_def,
        )
        .expect("circular reference present");
        assert_eq!(field_names(&result), ["id", "name"]);
    }

    #[test]
    fn from_def_converts_datetime_types() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub name: String,
            pub created_at: DateTimeWithTimeZone,
            pub memo: BelongsTo<memo::Entity>
        }";
        let result = generate_inline_relation_type_from_def(
            &syn::Ident::new("MemoSchema", proc_macro2::Span::call_site()),
            &rel("user", "BelongsTo", quote!(super::user::Schema)),
            &module_path(&MEMO_MODULE),
            None,
            model_def,
        )
        .expect("circular reference present");

        let created_at = result
            .fields
            .iter()
            .find(|f| f.name == "created_at")
            .expect("created_at field should exist");
        insta::assert_snapshot!("from_def_created_at_type", created_at.ty.to_string());
    }

    // ── generate_inline_relation_type_no_relations_from_def ─────────────

    #[test]
    fn no_relations_from_def_strips_relations() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub title: String,
            pub user: BelongsTo<user::Entity>,
            pub comments: HasMany<comment::Entity>
        }";
        let result = generate_inline_relation_type_no_relations_from_def(
            &syn::Ident::new("UserSchema", proc_macro2::Span::call_site()),
            &rel("memos", "HasMany", quote!(super::memo::Schema)),
            &[],
            None,
            model_def,
        )
        .expect("plain fields remain");

        assert_eq!(result.type_name.to_string(), "UserSchema_Memos");
        assert_eq!(field_names(&result), ["id", "title"]);
    }

    #[test]
    fn no_relations_from_def_skips_serde_skip_fields() {
        let model_def = r"pub struct Model {
            pub id: i32,
            #[serde(skip)]
            pub internal: String,
            pub name: String
        }";
        let result = generate_inline_relation_type_no_relations_from_def(
            &syn::Ident::new("TestSchema", proc_macro2::Span::call_site()),
            &rel("items", "HasMany", quote!(super::item::Schema)),
            &[],
            None,
            model_def,
        )
        .expect("plain fields remain");
        assert_eq!(field_names(&result), ["id", "name"]);
    }

    #[test]
    fn no_relations_from_def_schema_name_override_names_the_inline_type() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub title: String
        }";
        let result = generate_inline_relation_type_no_relations_from_def(
            &syn::Ident::new("Schema", proc_macro2::Span::call_site()),
            &rel("memos", "HasMany", quote!(super::memo::Schema)),
            &[],
            Some("UserSchema"),
            model_def,
        )
        .expect("plain fields remain");
        assert_eq!(result.type_name.to_string(), "UserSchema_Memos");
    }

    #[test]
    fn no_relations_from_def_converts_datetime_types() {
        let model_def = r"pub struct Model {
            pub id: i32,
            pub title: String,
            pub created_at: DateTimeWithTimeZone,
            pub updated_at: Option<DateTimeWithTimeZone>,
            pub user: BelongsTo<user::Entity>
        }";
        let result = generate_inline_relation_type_no_relations_from_def(
            &syn::Ident::new("UserSchema", proc_macro2::Span::call_site()),
            &rel("memos", "HasMany", quote!(super::memo::Schema)),
            &[],
            None,
            model_def,
        )
        .expect("plain fields remain");

        let ty_of = |name: &str| {
            result
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{name} field should exist"))
                .ty
                .to_string()
        };
        insta::assert_snapshot!(
            "no_relations_datetime_types",
            format!(
                "created_at: {}\nupdated_at: {}",
                ty_of("created_at"),
                ty_of("updated_at"),
            )
        );
    }

    // ── File-lookup variants (CARGO_MANIFEST_DIR + temp project) ────────

    #[test]
    #[serial]
    fn file_lookup_generates_inline_type_for_circular_model() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let models_dir = temp_dir.path().join("src").join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(
            models_dir.join("user.rs"),
            r"
    pub struct Model {
        pub id: i32,
        pub name: String,
        pub memo: BelongsTo<memo::Entity>,
    }
    ",
        )
        .unwrap();

        let result = with_manifest_dir(temp_dir.path(), || {
            generate_inline_relation_type(
                &syn::Ident::new("MemoSchema", proc_macro2::Span::call_site()),
                &rel("user", "BelongsTo", quote!(crate::models::user::Schema)),
                &module_path(&MEMO_MODULE),
                None,
            )
        })
        .expect("circular reference present");

        assert_eq!(result.type_name.to_string(), "MemoSchema_User");
        assert_eq!(field_names(&result), ["id", "name"]);
    }

    #[test]
    #[serial]
    fn file_lookup_no_relations_strips_relations() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let models_dir = temp_dir.path().join("src").join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        std::fs::write(
            models_dir.join("memo.rs"),
            r"
    pub struct Model {
        pub id: i32,
        pub title: String,
        pub user: BelongsTo<user::Entity>,
        pub comments: HasMany<comment::Entity>,
    }
    ",
        )
        .unwrap();

        let result = with_manifest_dir(temp_dir.path(), || {
            generate_inline_relation_type_no_relations(
                &syn::Ident::new("UserSchema", proc_macro2::Span::call_site()),
                &rel("memos", "HasMany", quote!(crate::models::memo::Schema)),
                &module_path(&["crate", "models", "user"]),
                None,
            )
        })
        .expect("plain fields remain");

        assert_eq!(result.type_name.to_string(), "UserSchema_Memos");
        assert_eq!(field_names(&result), ["id", "title"]);
    }

    #[test]
    #[serial]
    fn file_lookup_missing_model_file_returns_none() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp_dir.path().join("src")).unwrap();

        let result = with_manifest_dir(temp_dir.path(), || {
            generate_inline_relation_type(
                &syn::Ident::new("TestSchema", proc_macro2::Span::call_site()),
                &rel(
                    "user",
                    "BelongsTo",
                    quote!(crate::models::nonexistent::Schema),
                ),
                &module_path(&["crate"]),
                None,
            )
        });
        assert!(result.is_none());
    }

    #[test]
    #[serial]
    fn file_lookup_no_relations_missing_model_file_returns_none() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp_dir.path().join("src")).unwrap();

        let result = with_manifest_dir(temp_dir.path(), || {
            generate_inline_relation_type_no_relations(
                &syn::Ident::new("TestSchema", proc_macro2::Span::call_site()),
                &rel(
                    "items",
                    "HasMany",
                    quote!(crate::models::nonexistent::Schema),
                ),
                &[],
                None,
            )
        });
        assert!(result.is_none());
    }
