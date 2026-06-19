    use quote::quote;
    use rstest::rstest;

    use super::*;

    fn ident(name: &str) -> syn::Ident {
        syn::Ident::new(name, proc_macro2::Span::call_site())
    }

    fn fields(src: &str) -> syn::FieldsNamed {
        syn::parse_str(src).unwrap()
    }

    fn required(def: &str, field: &str) -> bool {
        analyze_circular_refs(&[], def)
            .circular_field_required
            .get(field)
            .copied()
            .unwrap_or(false)
    }

    #[rstest]
    #[case(&["crate", "models", "memo"], r"pub struct UserSchema { pub id: i32, pub memos: HasMany<memo::Entity>, }", vec![])]
    #[case(&["crate", "models", "user"], r"pub struct MemoSchema { pub id: i32, pub user: BelongsTo<user::Entity>, }", vec!["user".to_string()])]
    #[case(&["crate", "models", "user"], r"pub struct MemoSchema { pub id: i32, pub user: HasOne<user::Entity>, }", vec!["user".to_string()])]
    #[case(&["crate", "models", "user"], r"pub struct MemoSchema { pub id: i32, pub user: Box<user::Schema>, }", vec!["user".to_string()])]
    #[case(&["crate", "models", "memo"], r"pub struct UserSchema { pub id: i32, pub name: String, }", vec![])]
    fn test_detect_circular_fields(
        #[case] source_module_path: &[&str],
        #[case] related_schema_def: &str,
        #[case] expected: Vec<String>,
    ) {
        let module_path: Vec<String> = source_module_path.iter().map(ToString::to_string).collect();
        assert_eq!(
            analyze_circular_refs(&module_path, related_schema_def).circular_fields,
            expected
        );
    }

    #[test]
    fn test_detect_circular_fields_invalid_struct() {
        assert!(
            analyze_circular_refs(&["crate".to_string()], "not valid rust")
                .circular_fields
                .is_empty()
        );
    }

    #[test]
    fn test_detect_circular_fields_unnamed_fields() {
        let path = vec![
            "crate".to_string(),
            "models".to_string(),
            "test".to_string(),
        ];
        assert!(
            analyze_circular_refs(&path, "pub struct TupleStruct(i32, String);")
                .circular_fields
                .is_empty()
        );
    }

    #[rstest]
    #[case(
        r"pub struct Model { pub id: i32, pub user: BelongsTo<user::Entity>, }",
        true
    )]
    #[case(
        r"pub struct Model { pub id: i32, pub user: HasOne<user::Entity>, }",
        true
    )]
    #[case(r"pub struct Model { pub id: i32, pub name: String, }", false)]
    #[case(
        r"pub struct Model { pub id: i32, pub items: HasMany<item::Entity>, }",
        false
    )]
    fn test_has_fk_relations(#[case] model_def: &str, #[case] expected: bool) {
        assert_eq!(
            analyze_circular_refs(&[], model_def).has_fk_relations,
            expected
        );
    }

    #[test]
    fn test_has_fk_relations_invalid_struct() {
        assert!(!analyze_circular_refs(&[], "not valid rust").has_fk_relations);
    }

    #[test]
    fn test_has_fk_relations_unnamed_fields() {
        assert!(
            !analyze_circular_refs(&[], "pub struct TupleStruct(i32, String);").has_fk_relations
        );
    }

    #[test]
    fn test_is_circular_relation_required_invalid_struct() {
        assert!(!required("not valid rust", "user"));
    }

    #[test]
    fn test_is_circular_relation_required_unnamed_fields() {
        assert!(!required("pub struct TupleStruct(i32, String);", "user"));
    }

    #[test]
    fn test_is_circular_relation_required_field_not_found() {
        assert!(!required(
            "pub struct Model { pub id: i32, pub name: String, }",
            "nonexistent"
        ));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_many() {
        let ty: syn::Type = syn::parse_str("HasMany<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("users"),
                &[],
                &fields("{ pub id: i32 }")
            )
            .to_string()
            .contains("users : vec ! []")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub user_id: Option<i32> }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_unknown_type() {
        let ty: syn::Type = syn::parse_str("SomeUnknownType<T>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("field"),
                &[],
                &fields("{ pub id: i32 }")
            )
            .to_string()
            .contains("Default :: default ()")
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_invalid_struct() {
        assert!(
            generate_inline_struct_construction(
                &quote! { user::Schema },
                "not valid rust",
                &[],
                "model"
            )
            .to_string()
            .contains("From")
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_tuple_struct() {
        assert!(
            generate_inline_struct_construction(
                &quote! { user::Schema },
                "pub struct TupleStruct(i32, String);",
                &[],
                "model"
            )
            .to_string()
            .contains("From")
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_with_fields() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, pub name: String, }",
            &[],
            "r",
        )
        .to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("name : r . name"));
    }

    #[test]
    fn test_generate_inline_struct_construction_with_circular_field() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, pub memos: HasMany<memo::Entity>, }",
            &["memos".to_string()],
            "r",
        )
        .to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("memos : vec ! []"));
    }

    #[test]
    fn test_generate_inline_struct_construction_skip_serde_skip_fields() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, #[serde(skip)] pub internal: String, }",
            &[],
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("internal : r . internal"));
    }

    #[test]
    fn test_generate_inline_type_construction_invalid_struct() {
        assert!(
            generate_inline_type_construction(
                &ident("TestInline"),
                &["id".to_string()],
                "not valid rust",
                "model"
            )
            .to_string()
            .contains("Default :: default ()")
        );
    }

    #[test]
    fn test_generate_inline_type_construction_tuple_struct() {
        assert!(
            generate_inline_type_construction(
                &ident("TestInline"),
                &["id".to_string()],
                "pub struct TupleStruct(i32, String);",
                "model"
            )
            .to_string()
            .contains("Default :: default ()")
        );
    }

    #[test]
    fn test_generate_inline_type_construction_with_fields() {
        let output = generate_inline_type_construction(
            &ident("UserInline"),
            &["id".to_string(), "name".to_string()],
            r"pub struct Model { pub id: i32, pub name: String, pub email: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("UserInline"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("name : r . name"));
        assert!(!output.contains("email : r . email"));
    }

    #[test]
    fn test_generate_inline_type_construction_skips_relations() {
        let output = generate_inline_type_construction(
            &ident("UserInline"),
            &["id".to_string(), "memos".to_string()],
            r"pub struct Model { pub id: i32, pub memos: HasMany<memo::Entity>, }",
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("memos : r . memos"));
    }

    #[test]
    fn test_circular_field_required_has_one_with_required_fk() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, pub user_id: i32, #[sea_orm(belongs_to = "super::user::Entity", from = "Column::UserId", to = "super::user::Column::Id")] pub user: HasOne<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_belongs_to_with_optional_fk() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, pub user_id: Option<i32>, #[sea_orm(belongs_to = "super::user::Entity", from = "Column::UserId", to = "super::user::Column::Id")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_non_relation_field() {
        assert!(!required(
            r"pub struct Model { pub id: i32, pub name: String, }",
            "name"
        ));
    }

    #[test]
    fn test_circular_field_required_field_without_ident() {
        assert!(!required(
            r"pub struct Model { pub id: i32, }",
            "nonexistent_field"
        ));
    }

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_optional() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub user_id: Option<i32> }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub user_id: i32 }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_no_fk_found() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        assert!(
            generate_default_for_relation_field(
                &ty,
                &ident("user"),
                &[],
                &fields("{ pub id: i32 }")
            )
            .to_string()
            .contains("user : None")
        );
    }

    #[test]
    fn test_circular_fields_empty_module_path() {
        assert!(
            analyze_circular_refs(&[], "pub struct Schema { pub id: i32 }")
                .circular_fields
                .is_empty()
        );
    }

    #[test]
    fn test_circular_fields_option_box_pattern() {
        let path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        assert_eq!(
            analyze_circular_refs(
                &path,
                r"pub struct UserSchema { pub id: i32, pub memo: Option<Box<memo::Schema>>, }"
            )
            .circular_fields,
            vec!["memo".to_string()]
        );
    }

    #[test]
    fn test_circular_fields_schema_suffix_pattern() {
        let path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        assert_eq!(
            analyze_circular_refs(
                &path,
                r"pub struct UserSchema { pub id: i32, pub memo: Box<MemoSchema>, }"
            )
            .circular_fields,
            vec!["memo".to_string()]
        );
    }

    #[test]
    fn test_circular_fields_field_without_ident() {
        let path = vec!["crate".to_string(), "test".to_string()];
        assert!(
            analyze_circular_refs(&path, r"pub struct Schema { pub id: i32, }")
                .circular_fields
                .is_empty()
        );
    }

    #[test]
    fn test_generate_inline_struct_construction_with_belongs_to_relation() {
        let output = generate_inline_struct_construction(&quote! { memo::Schema }, r"pub struct MemoSchema { pub id: i32, pub user_id: i32, pub user: BelongsTo<user::Entity>, }", &[], "r").to_string();
        assert!(output.contains("memo :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("user_id : r . user_id"));
        assert!(output.contains("user : None"));
    }

    #[test]
    fn test_generate_inline_struct_construction_with_has_one_relation() {
        let output = generate_inline_struct_construction(
            &quote! { user::Schema },
            r"pub struct UserSchema { pub id: i32, pub profile: HasOne<profile::Entity>, }",
            &[],
            "r",
        )
        .to_string();
        assert!(output.contains("user :: Schema"));
        assert!(output.contains("id : r . id"));
        assert!(output.contains("profile : None"));
    }

    #[test]
    fn test_generate_inline_type_construction_skips_serde_skip() {
        let output = generate_inline_type_construction(
            &ident("TestInline"),
            &["id".to_string(), "internal".to_string()],
            r"pub struct Model { pub id: i32, #[serde(skip)] pub internal: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("internal : r . internal"));
    }

    #[test]
    fn test_generate_inline_type_construction_empty_included_fields() {
        let output = generate_inline_type_construction(
            &ident("EmptyInline"),
            &[],
            r"pub struct Model { pub id: i32, pub name: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("EmptyInline"));
        assert!(!output.contains("id : r . id"));
        assert!(!output.contains("name : r . name"));
    }

    #[test]
    fn test_generate_inline_type_construction_field_not_in_included() {
        let output = generate_inline_type_construction(
            &ident("PartialInline"),
            &["id".to_string()],
            r"pub struct Model { pub id: i32, pub name: String, pub email: String, }",
            "r",
        )
        .to_string();
        assert!(output.contains("id : r . id"));
        assert!(!output.contains("name : r . name"));
        assert!(!output.contains("email : r . email"));
    }

    #[test]
    fn test_circular_field_required_belongs_to_with_from_attr_required_fk() {
        assert!(required(
            r#"pub struct Model { pub id: i32, pub user_id: i32, #[sea_orm(from = "user_id")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_belongs_to_with_from_attr_optional_fk() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, pub user_id: Option<i32>, #[sea_orm(from = "user_id")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_circular_field_required_has_one_with_from_attr_required_fk() {
        assert!(required(
            r#"pub struct Model { pub id: i32, pub profile_id: i64, #[sea_orm(from = "profile_id")] pub profile: HasOne<profile::Entity>, }"#,
            "profile"
        ));
    }

    #[test]
    fn test_circular_field_required_from_attr_fk_field_not_found() {
        assert!(!required(
            r#"pub struct Model { pub id: i32, #[sea_orm(from = "nonexistent_field")] pub user: BelongsTo<user::Entity>, }"#,
            "user"
        ));
    }

    #[test]
    fn test_generate_default_for_relation_field_belongs_to_with_from_attr_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "user_id")]);
        let output = generate_default_for_relation_field(
            &ty,
            &ident("user"),
            &[attr],
            &fields("{ pub user_id: i32 }"),
        )
        .to_string();
        assert!(output.contains("__parent_stub__"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_with_from_attr_required() {
        let ty: syn::Type = syn::parse_str("HasOne<profile::Entity>").unwrap();
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "profile_id")]);
        let output = generate_default_for_relation_field(
            &ty,
            &ident("profile"),
            &[attr],
            &fields("{ pub profile_id: i64 }"),
        )
        .to_string();
        assert!(output.contains("__parent_stub__"));
        assert!(output.contains("Box :: new"));
    }

    #[test]
    fn test_generate_default_for_relation_field_has_one_with_from_attr_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<profile::Entity>").unwrap();
        let attr: syn::Attribute = syn::parse_quote!(#[sea_orm(from = "profile_id")]);
        let output = generate_default_for_relation_field(
            &ty,
            &ident("profile"),
            &[attr],
            &fields("{ pub profile_id: Option<i64> }"),
        )
        .to_string();
        assert!(output.contains("profile : None"));
    }
