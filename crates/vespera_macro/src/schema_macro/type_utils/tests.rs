use rstest::rstest;

use super::*;
fn empty_type_path() -> syn::Type {
    syn::Type::Path(syn::TypePath {
        attrs: Vec::new(),
        qself: None,
        path: syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::new(),
        },
    })
}

#[rstest]
#[case("hello", "Hello")]
#[case("world", "World")]
#[case("", "")]
#[case("a", "A")]
#[case("ABC", "ABC")]
#[case("camelCase", "CamelCase")]
fn test_capitalize_first(#[case] input: &str, #[case] expected: &str) {
    assert_eq!(capitalize_first(input), expected);
}

#[rstest]
#[case("comments", "Comments")]
#[case("target_user_notifications", "TargetUserNotifications")]
#[case("memo_comments", "MemoComments")]
#[case("", "")]
#[case("a", "A")]
#[case("user_id", "UserId")]
#[case("ABC", "ABC")]
fn test_snake_to_pascal_case(#[case] input: &str, #[case] expected: &str) {
    assert_eq!(snake_to_pascal_case(input), expected);
}

#[rstest]
#[case("bool", true)]
#[case("i32", true)]
#[case("String", true)]
#[case("Vec", true)]
#[case("Option", true)]
#[case("HashMap", true)]
#[case("DateTime", true)]
#[case("Uuid", true)]
#[case("Decimal", true)]
#[case("DateTimeWithTimeZone", true)]
#[case("CustomType", false)]
#[case("MyStruct", false)]
fn test_is_primitive_or_known_type(#[case] name: &str, #[case] expected: bool) {
    assert_eq!(is_primitive_or_known_type(name), expected);
}

#[test]
fn test_extract_type_name_simple() {
    let ty: syn::Type = syn::parse_str("User").unwrap();
    let name = extract_type_name(&ty).unwrap();
    assert_eq!(name, "User");
}

#[test]
fn test_extract_type_name_with_path() {
    let ty: syn::Type = syn::parse_str("crate::models::User").unwrap();
    let name = extract_type_name(&ty).unwrap();
    assert_eq!(name, "User");
}

#[test]
fn test_extract_type_name_non_path_error() {
    let ty: syn::Type = syn::parse_str("&str").unwrap();
    let result = extract_type_name(&ty);
    assert!(result.is_err());
}

#[test]
fn test_is_option_type_true() {
    let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
    assert!(is_option_type(&ty));
}

#[test]
fn test_is_option_type_false() {
    let ty: syn::Type = syn::parse_str("String").unwrap();
    assert!(!is_option_type(&ty));
}

#[test]
fn test_is_option_type_vec_false() {
    let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
    assert!(!is_option_type(&ty));
}

#[test]
fn test_is_option_type_non_path() {
    let ty: syn::Type = syn::parse_str("&str").unwrap();
    assert!(!is_option_type(&ty));
}

#[test]
fn test_is_option_type_empty_path() {
    let ty = empty_type_path();
    assert!(!is_option_type(&ty));
}

#[test]
fn test_is_seaorm_relation_type_has_one() {
    let ty: syn::Type = syn::parse_str("HasOne<User>").unwrap();
    assert!(is_seaorm_relation_type(&ty));
}

#[test]
fn test_is_seaorm_relation_type_has_many() {
    let ty: syn::Type = syn::parse_str("HasMany<Post>").unwrap();
    assert!(is_seaorm_relation_type(&ty));
}

#[test]
fn test_is_seaorm_relation_type_belongs_to() {
    let ty: syn::Type = syn::parse_str("BelongsTo<User>").unwrap();
    assert!(is_seaorm_relation_type(&ty));
}

#[test]
fn test_is_seaorm_relation_type_regular_type() {
    let ty: syn::Type = syn::parse_str("String").unwrap();
    assert!(!is_seaorm_relation_type(&ty));
}

#[test]
fn test_is_seaorm_relation_type_non_path() {
    let ty: syn::Type = syn::parse_str("&str").unwrap();
    assert!(!is_seaorm_relation_type(&ty));
}

#[test]
fn test_is_seaorm_relation_type_empty_path() {
    let ty = empty_type_path();
    assert!(!is_seaorm_relation_type(&ty));
}

#[test]
fn test_is_seaorm_model_with_sea_orm_attr() {
    let struct_item: syn::ItemStruct = syn::parse_str(
        r#"
            #[sea_orm(table_name = "users")]
            struct Model {
                id: i32,
            }
        "#,
    )
    .unwrap();
    assert!(is_seaorm_model(&struct_item));
}

#[test]
fn test_is_seaorm_model_with_qualified_attr() {
    let struct_item: syn::ItemStruct = syn::parse_str(
        r"
            #[sea_orm::model]
            struct Model {
                id: i32,
            }
        ",
    )
    .unwrap();
    assert!(is_seaorm_model(&struct_item));
}

#[test]
fn test_is_seaorm_model_regular_struct() {
    let struct_item: syn::ItemStruct = syn::parse_str(
        r"
            #[derive(Debug)]
            struct User {
                id: i32,
            }
        ",
    )
    .unwrap();
    assert!(!is_seaorm_model(&struct_item));
}

#[test]
fn test_extract_module_path_simple() {
    let ty: syn::Type = syn::parse_str("User").unwrap();
    let result = extract_module_path(&ty);
    assert_eq!(result, Vec::<String>::new());
}

#[test]
fn test_extract_module_path_qualified() {
    let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
    let result = extract_module_path(&ty);
    assert_eq!(result, vec!["crate", "models", "user"]);
}

#[test]
fn test_extract_module_path_non_path_type() {
    let ty: syn::Type = syn::parse_str("&str").unwrap();
    let result = extract_module_path(&ty);
    assert_eq!(result, Vec::<String>::new());
}

#[test]
fn test_resolve_type_to_absolute_path_non_path_type() {
    let ty: syn::Type = syn::parse_str("&str").unwrap();
    let module_path = vec!["crate".to_string(), "models".to_string()];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert!(output.contains("& str"));
}

#[test]
fn test_resolve_type_to_absolute_path_already_qualified() {
    let ty: syn::Type = syn::parse_str("crate::models::User").unwrap();
    let module_path = vec!["crate".to_string(), "other".to_string()];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert!(output.contains("crate :: models :: User"));
}

#[test]
fn test_resolve_type_to_absolute_path_primitive() {
    let ty: syn::Type = syn::parse_str("String").unwrap();
    let module_path = vec!["crate".to_string(), "models".to_string()];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert_eq!(output.trim(), "String");
}

#[test]
fn test_resolve_type_to_absolute_path_known_type_with_generic_args() {
    let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
    let module_path = vec!["crate".to_string(), "models".to_string()];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert_eq!(output.trim(), "Option < String >");
}

#[test]
fn test_resolve_type_to_absolute_path_decimal() {
    let ty: syn::Type = syn::parse_str("Decimal").unwrap();
    let module_path = vec![
        "crate".to_string(),
        "models".to_string(),
        "review".to_string(),
    ];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    // Decimal is a known type — must NOT be resolved to crate::models::review::Decimal
    assert_eq!(output.trim(), "Decimal");
}

#[test]
fn test_resolve_type_to_absolute_path_json_alias_uses_public_path() {
    let ty: syn::Type = syn::parse_str("Json").unwrap();
    let module_path = vec![
        "crate".to_string(),
        "models".to_string(),
        "json_case".to_string(),
    ];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert_eq!(output.trim(), "vespera :: serde_json :: Value");
}

#[test]
fn test_resolve_type_to_absolute_path_known_container_normalizes_inner_json_alias() {
    let ty: syn::Type = syn::parse_str("HashMap<String, Json>").unwrap();
    let module_path = vec![
        "crate".to_string(),
        "models".to_string(),
        "json_case".to_string(),
    ];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert!(output.contains("HashMap < String , vespera :: serde_json :: Value >"));
    assert!(!output.contains("crate :: models :: json_case :: Json"));
}

#[test]
fn test_resolve_type_to_absolute_path_custom_type() {
    let ty: syn::Type = syn::parse_str("MemoStatus").unwrap();
    let module_path = vec![
        "crate".to_string(),
        "models".to_string(),
        "memo".to_string(),
    ];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert!(output.contains("crate :: models :: memo :: MemoStatus"));
}

#[test]
fn test_resolve_type_to_absolute_path_empty_module() {
    let ty: syn::Type = syn::parse_str("CustomType").unwrap();
    let module_path: Vec<String> = vec![];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert_eq!(output.trim(), "CustomType");
}

#[test]
fn test_resolve_type_to_absolute_path_with_generics() {
    let ty: syn::Type = syn::parse_str("CustomType<T>").unwrap();
    let module_path = vec!["crate".to_string(), "models".to_string()];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert_eq!(
        output,
        "crate :: models :: CustomType < crate :: models :: T >"
    );
}

#[test]
fn test_resolve_type_to_absolute_path_empty_segments() {
    let ty = empty_type_path();
    let module_path = vec!["crate".to_string()];
    let tokens = resolve_type_to_absolute_path(&ty, &module_path);
    let output = tokens.to_string();
    assert_eq!(output.trim(), "");
}

#[rstest]
#[case("HashMap<String, i32>", true)]
#[case("BTreeMap<String, i32>", true)]
#[case("String", false)]
#[case("Vec<String>", false)]
fn test_is_map_type(#[case] type_str: &str, #[case] expected: bool) {
    let ty: syn::Type = syn::parse_str(type_str).unwrap();
    assert_eq!(is_map_type(&ty), expected);
}

#[rstest]
#[case("String", Some(serde_json::Value::String(String::new())))]
#[case("i32", Some(serde_json::Value::Number(serde_json::Number::from(0))))]
#[case(
    "Decimal",
    Some(serde_json::Value::Number(serde_json::Number::from(0)))
)]
#[case("bool", Some(serde_json::Value::Bool(false)))]
#[case("Option<String>", Some(serde_json::Value::Null))]
#[case("f64", Some(serde_json::Value::Number(serde_json::Number::from_f64(0.0).unwrap())))]
#[case("CustomType", None)]
fn test_get_type_default(#[case] type_str: &str, #[case] expected: Option<serde_json::Value>) {
    let ty: syn::Type = syn::parse_str(type_str).unwrap();
    let result = get_type_default(&ty);
    match expected {
        Some(exp) => {
            assert!(result.is_some());
            let res = result.unwrap();
            assert_eq!(res, exp);
        }
        None => assert!(result.is_none()),
    }
}

#[test]
fn test_is_primitive_like_true() {
    let ty: syn::Type = syn::parse_str("String").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_vec_of_primitives() {
    let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_option_of_primitives() {
    let ty: syn::Type = syn::parse_str("Option<i32>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_custom_type() {
    let ty: syn::Type = syn::parse_str("User").unwrap();
    assert!(!is_primitive_like(&ty));
}

// Edge case tests for type_utils functions

#[test]
fn test_extract_type_name_empty_path_error() {
    let ty = empty_type_path();
    let result = extract_type_name(&ty);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("type path has no segments")
    );
}

#[test]
fn test_is_map_type_empty_path() {
    let ty = empty_type_path();
    assert!(!is_map_type(&ty));
}

#[test]
fn test_is_primitive_like_vec_string() {
    let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_vec_i32() {
    let ty: syn::Type = syn::parse_str("Vec<i32>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_option_string() {
    let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_option_bool() {
    let ty: syn::Type = syn::parse_str("Option<bool>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_vec_of_custom_type() {
    // Vec is a known type, so Vec<User> is considered primitive-like
    let ty: syn::Type = syn::parse_str("Vec<User>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_option_of_custom_type() {
    // Option is a known type, so Option<User> is considered primitive-like
    let ty: syn::Type = syn::parse_str("Option<User>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_nested_vec_option() {
    let ty: syn::Type = syn::parse_str("Vec<Option<String>>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_nested_option_vec() {
    let ty: syn::Type = syn::parse_str("Option<Vec<i32>>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[test]
fn test_is_primitive_like_vec_of_datetime() {
    let ty: syn::Type = syn::parse_str("Vec<DateTime<Utc>>").unwrap();
    assert!(is_primitive_like(&ty));
}

#[rstest]
#[case::reference("&str", "& str")]
#[case::leading_colon("::crate::models::CustomType", ":: crate :: models :: CustomType")]
#[case::qualified("crate::models::CustomType", "crate :: models :: CustomType")]
#[case::lifetime_args("Borrowed<'a>", "Borrowed < 'a >")]
#[case::paren("(String)", "(String)")]
#[case::unit_tuple("()", "()")]
#[case::macro_type("m!()", "m ! ()")]
#[case::turbofish_args("Vec::<String>", "Vec :: < String >")]
fn resolve_type_without_module_keeps_shape(#[case] input: &str, #[case] expected: &str) {
    let ty: syn::Type = syn::parse_str(input).unwrap();
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &[]).to_string(),
        expected
    );
}

#[test]
fn parenthesized_path_arguments_are_kept_verbatim() {
    let bound: syn::TraitBound = syn::parse_str("Fn(Local) -> String").unwrap();
    let path = bound.path;
    let ty = syn::Type::Path(syn::TypePath {
        attrs: Vec::new(),
        qself: None,
        path,
    });
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &[]).to_string(),
        "Fn (Local) -> String"
    );
}

#[rstest]
#[case::bare_uuid("Uuid", "Uuid")]
#[case::date_without_sea_orm_scope("Date", "Date")]
#[case::sea_orm_qualified_alias(
    "sea_orm::prelude::DateTimeUtc",
    "vespera :: chrono :: DateTime < vespera :: chrono :: Utc >"
)]
#[case::external_crate_path("uuid::Uuid", "uuid :: Uuid")]
#[case::generic_param_kept("T", "T")]
fn resolve_type_unknown_module_cases(#[case] input: &str, #[case] expected: &str) {
    let ty: syn::Type = syn::parse_str(input).unwrap();
    let module = ["crate".to_string(), "nowhere".to_string()];
    assert_eq!(
        resolve_type_with_params(&ty, &module, &["T".to_string()]).to_string(),
        expected
    );
}

#[test]
fn test_resolve_type_to_absolute_path_leading_colon_and_empty_path() {
    let ty: syn::Type = syn::parse_str("::crate::models::User").unwrap();
    let tokens = resolve_type_to_absolute_path(&ty, &["ignored".to_string()]);
    assert!(tokens.to_string().contains(":: crate :: models :: User"));

    let empty_ty = empty_type_path();
    let tokens = resolve_type_to_absolute_path(&empty_ty, &["crate".to_string()]);
    assert_eq!(tokens.to_string().trim(), "");
}

#[test]
fn first_generic_type_arg_handles_no_arguments_and_lifetime_only() {
    let plain: syn::Type = syn::parse_str("Option").unwrap();
    let syn::Type::Path(plain) = plain else {
        panic!("fixture is a path");
    };
    assert!(first_generic_type_arg(plain.path.segments.last().unwrap()).is_none());

    let lifetime: syn::Type = syn::parse_str("Borrowed<'a>").unwrap();
    let syn::Type::Path(lifetime) = lifetime else {
        panic!("fixture is a path");
    };
    assert!(first_generic_type_arg(lifetime.path.segments.last().unwrap()).is_none());
}

#[test]
fn normalize_token_str_reuses_whitespace_free_string() {
    assert_eq!(normalize_token_str(&"AlreadyCompact"), "AlreadyCompact");
}

#[test]
fn option_inner_rejects_wrapper_without_generic_arguments() {
    let ty: syn::Type = syn::parse_str("Option").unwrap();
    assert!(option_inner(&ty).is_none());
}

#[rstest]
#[case::uuid("Uuid", serde_json::json!("00000000-0000-0000-0000-000000000000"))]
#[case::datetime("DateTimeUtc", serde_json::json!("1970-01-01T00:00:00+00:00"))]
#[case::naive_datetime("NaiveDateTime", serde_json::json!("1970-01-01T00:00:00"))]
#[case::naive_date("NaiveDate", serde_json::json!("1970-01-01"))]
#[case::naive_time("NaiveTime", serde_json::json!("00:00:00"))]
#[case::time("Time", serde_json::json!("00:00:00"))]
fn type_default_returns_temporal_and_uuid_sentinels(
    #[case] input: &str,
    #[case] expected: serde_json::Value,
) {
    let ty: syn::Type = syn::parse_str(input).unwrap();
    assert_eq!(get_type_default(&ty), Some(expected));
}

/// SeaORM's `Json` / `JsonValue` aliases and a bare `Value` all become the
/// public `serde_json::Value` path — but a `Json<T>` (axum's body wrapper)
/// is a different type and is resolved like any other user type.
#[rstest]
#[case::json("Json", "vespera :: serde_json :: Value")]
#[case::json_value("JsonValue", "vespera :: serde_json :: Value")]
#[case::value("Value", "vespera :: serde_json :: Value")]
#[case::option_json_value("Option<JsonValue>", "Option < vespera :: serde_json :: Value >")]
#[case::vec_in_generic("Vec<JsonValue>", "Vec < vespera :: serde_json :: Value >")]
#[case::json_wrapper(
    "Json<Payload>",
    "crate :: models :: doc :: Json < crate :: models :: doc :: Payload >"
)]
fn json_aliases_resolve_to_public_value_path(#[case] src: &str, #[case] expected: &str) {
    let ty: syn::Type = syn::parse_str(src).unwrap();
    let module_path = vec!["crate".to_string(), "models".to_string(), "doc".to_string()];
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &module_path).to_string(),
        expected
    );
}
