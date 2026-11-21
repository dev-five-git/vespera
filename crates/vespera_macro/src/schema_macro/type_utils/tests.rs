use rstest::rstest;

use super::*;

/// A crate-local `Decimal` is not rust_decimal's: its `"0"` default is not
/// advertised. The SeaORM `Date` imported in the call-site module still is.
#[test]
fn ecosystem_default_names_follow_the_source_scope() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub struct Decimal(pub i64);\nmod sea;\nmod own;\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/sea.rs"),
        "use sea_orm::prelude::Date;\npub struct Holder { pub d: Date }\n",
    )
    .unwrap();
    // `Date` imported from a crate that is not an ecosystem one (R20-S4-L4,
    // round 20): the call site's own `Date`, so no ecosystem default.
    std::fs::write(
        dir.path().join("src/own.rs"),
        "use my_crate::Date;\npub struct Holder { pub d: Date }\n",
    )
    .unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    let decimal: syn::Type = syn::parse_quote!(Decimal);
    let date: syn::Type = syn::parse_quote!(Date);
    let root = module(&["crate"]);
    assert_eq!(get_type_default(&decimal, &root), None);
    // Not imported here, so the name cannot be shown to be a user type:
    // the ecosystem default stays (the `Unknown` resolution).
    assert_eq!(
        get_type_default(&date, &root),
        Some(serde_json::json!("1970-01-01"))
    );
    assert_eq!(
        get_type_default(&date, &module(&["crate", "sea"])),
        Some(serde_json::json!("1970-01-01"))
    );
    assert_eq!(get_type_default(&date, &module(&["crate", "own"])), None);
}

/// An absolute module path from its segments.
fn module(segments: &[&str]) -> Vec<String> {
    segments.iter().map(|s| (*s).to_string()).collect()
}

/// The source module decides a crate-local `Decimal`, whatever the macro's
/// call site is (`vespera!` at the crate root scanning `models/order.rs`).
/// The root imports rust_decimal; `models::order` defines its own
/// `Decimal`, so the default is advertised only for the root's.
#[test]
fn ecosystem_default_resolves_in_the_source_module_not_the_call_site() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src/models")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "use rust_decimal::Decimal;\npub mod models;\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/models.rs"), "pub mod order;\n").unwrap();
    std::fs::write(
        dir.path().join("src/models/order.rs"),
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct Decimal(pub i64);\n",
    )
    .unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    let decimal: syn::Type = syn::parse_quote!(Decimal);
    // The macro is invoked at the crate root, but the struct lives in
    // `models::order`: its own `Decimal` gets no `"0"` default.
    assert_eq!(
        get_type_default(&decimal, &module(&["crate"])),
        Some(serde_json::json!("0"))
    );
    assert_eq!(
        get_type_default(&decimal, &module(&["crate", "models", "order"])),
        None
    );
}

/// The reverse: the call site defines the crate-local `Decimal`, while the
/// struct's module imports rust_decimal's. Its `"0"` default is advertised.
#[test]
fn ecosystem_default_reverse_case_uses_the_source_module() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("src/models")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub struct Decimal(pub i64);\npub mod models;\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/models.rs"), "pub mod order;\n").unwrap();
    std::fs::write(
        dir.path().join("src/models/order.rs"),
        "use rust_decimal::Decimal;\n",
    )
    .unwrap();
    let _manifest = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    let decimal: syn::Type = syn::parse_quote!(Decimal);
    assert_eq!(get_type_default(&decimal, &module(&["crate"])), None);
    assert_eq!(
        get_type_default(&decimal, &module(&["crate", "models", "order"])),
        Some(serde_json::json!("0"))
    );
}

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
#[case("DateTime<Local>", None)]
#[case("DateTimeLocal", None)]
#[case("bool", Some(serde_json::Value::Bool(false)))]
#[case("Option<String>", Some(serde_json::Value::Null))]
#[case("f64", Some(serde_json::Value::Number(serde_json::Number::from_f64(0.0).unwrap())))]
#[case("CustomType", None)]
fn test_get_type_default(#[case] type_str: &str, #[case] expected: Option<serde_json::Value>) {
    let ty: syn::Type = syn::parse_str(type_str).unwrap();
    let result = get_type_default(&ty, &[]);
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

/// R27-SM4-L2: `Item::Cursor` of `Page<Item>` is the generic's path. The
/// source module imports a different `Item`, which must not root it.
#[test]
fn generic_parameter_path_is_kept_despite_an_import_of_its_name() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod page; pub mod item;").unwrap();
    std::fs::write(src.join("models").join("item.rs"), "pub trait Item {}").unwrap();
    std::fs::write(
        src.join("models").join("page.rs"),
        "use crate::models::item::Item;",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "models", "page"].map(String::from);
    let ty: syn::Type = syn::parse_str("Item::Cursor").unwrap();
    assert_eq!(
        resolve_type_with_params(&ty, &module, &["Item".to_string()]).to_string(),
        "Item :: Cursor"
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
#[case::datetime_utc("DateTimeUtc", serde_json::json!("1970-01-01T00:00:00Z"))]
#[case::chrono_utc("DateTime<Utc>", serde_json::json!("1970-01-01T00:00:00Z"))]
#[case::with_time_zone("DateTimeWithTimeZone", serde_json::json!("1970-01-01T00:00:00+00:00"))]
#[case::chrono_fixed("chrono::DateTime<chrono::FixedOffset>", serde_json::json!("1970-01-01T00:00:00+00:00"))]
// SeaORM's bare `DateTime` is `NaiveDateTime`; `Decimal` serializes as a
// string.
#[case::sea_orm_datetime("DateTime", serde_json::json!("1970-01-01T00:00:00"))]
#[case::decimal("Decimal", serde_json::json!("0"))]
#[case::naive_datetime("NaiveDateTime", serde_json::json!("1970-01-01T00:00:00"))]
#[case::naive_date("NaiveDate", serde_json::json!("1970-01-01"))]
#[case::naive_time("NaiveTime", serde_json::json!("00:00:00"))]
#[case::time("Time", serde_json::json!("00:00:00"))]
// Every other `Default` a bare `#[serde(default)]` can name: wide and
// pointer-sized integers, collections and SeaORM's `Date` alias.
#[case::usize("usize", serde_json::json!(0))]
#[case::isize("isize", serde_json::json!(0))]
#[case::i128("i128", serde_json::json!(0))]
#[case::u128("u128", serde_json::json!(0))]
#[case::vec("Vec<String>", serde_json::json!([]))]
#[case::vec_deque("std::collections::VecDeque<u8>", serde_json::json!([]))]
#[case::hash_set("HashSet<String>", serde_json::json!([]))]
#[case::btree_set("BTreeSet<i32>", serde_json::json!([]))]
#[case::hash_map("HashMap<String, i32>", serde_json::json!({}))]
#[case::btree_map("std::collections::BTreeMap<String, String>", serde_json::json!({}))]
#[case::sea_orm_date("Date", serde_json::json!("1970-01-01"))]
fn type_default_returns_temporal_and_uuid_sentinels(
    #[case] input: &str,
    #[case] expected: serde_json::Value,
) {
    let ty: syn::Type = syn::parse_str(input).unwrap();
    assert_eq!(get_type_default(&ty, &[]), Some(expected));
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

/// Under a SeaORM prelude glob a bare `Value` is the prelude's (sea_query's)
/// `Value`, not `serde_json::Value`; `Json` is still `serde_json::Value`.
#[test]
fn sea_orm_prelude_value_is_not_serde_json_value() {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod sea;").unwrap();
    std::fs::write(
        src.join("sea.rs"),
        "use sea_orm::prelude::*;\npub struct Holder { pub v: Value, pub j: Json }\n",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let module = ["crate", "sea"].map(String::from);
    let value: syn::Type = syn::parse_quote!(Value);
    let json: syn::Type = syn::parse_quote!(Json);
    // SM4-M1 (round 38): the glob's path, not the bare primitive-list name
    // (which a call site without the glob does not resolve) nor serde_json's.
    assert_eq!(
        resolve_type_to_absolute_path(&value, &module).to_string(),
        "sea_orm :: prelude :: Value"
    );
    assert_eq!(
        resolve_type_to_absolute_path(&json, &module).to_string(),
        "vespera :: serde_json :: Value"
    );
}

/// chrono names map to `vespera::chrono` only bare; with arguments they are
/// not the chrono re-export and are kept as written.
#[rstest]
#[case::naive_datetime("NaiveDateTime", "vespera :: chrono :: NaiveDateTime")]
#[case::utc("Utc", "vespera :: chrono :: Utc")]
#[case::fixed_offset("Option<FixedOffset>", "Option < vespera :: chrono :: FixedOffset >")]
#[case::with_args("Local<u8>", "Local < u8 >")]
fn chrono_names_resolve_bare_only(#[case] src: &str, #[case] expected: &str) {
    let ty: syn::Type = syn::parse_str(src).unwrap();
    let module_path = vec!["crate".to_string(), "models".to_string(), "doc".to_string()];
    assert_eq!(
        resolve_type_to_absolute_path(&ty, &module_path).to_string(),
        expected
    );
}

/// R36-SM4-L2: a qualified-self path re-roots its self type and its trait
/// path; the associated segments stay as written, and a path without a trait
/// (`<T>::X`) re-roots only its self type.
#[rstest]
#[case::self_type_and_trait(
    "<Id as Keyed>::Out",
    "< crate :: keys :: Id as crate :: keys :: Keyed > :: Out"
)]
#[case::primitive_self_type("<i32 as Keyed>::Out", "< i32 as crate :: keys :: Keyed > :: Out")]
#[case::no_trait("<Id>::Out", "< crate :: keys :: Id > :: Out")]
fn qself_path_is_rerooted(#[case] ty: &str, #[case] expected: &str) {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models; pub mod keys;").unwrap();
    std::fs::write(
        src.join("keys.rs"),
        "pub trait Keyed { type Out; }
pub struct Id;",
    )
    .unwrap();
    std::fs::write(src.join("models.rs"), "pub mod file;").unwrap();
    std::fs::write(
        src.join("models").join("file.rs"),
        "use crate::keys::{Id, Keyed};",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let ty: syn::Type = syn::parse_str(ty).unwrap();
    let module = ["crate", "models", "file"].map(String::from);
    assert_eq!(
        resolve_type_with_params(&ty, &module, &[]).to_string(),
        expected
    );
}

/// L5 (round 14): an array length naming a constant of the source module is
/// re-rooted like the element type (`[u8; HASH_LEN]` was copied as written:
/// E0425 at the call site); literals, const generic parameters, unknown and
/// `::`-rooted names stay as written.
#[rstest]
#[case::local_const("[u8; HASH_LEN]", &[], "[u8 ; crate :: models :: file :: HASH_LEN]")]
#[case::imported_const("[u8; SHARED_LEN]", &[], "[u8 ; crate :: consts :: SHARED_LEN]")]
#[case::relative_path("[u8; super::file::HASH_LEN]", &[], "[u8 ; crate :: models :: file :: HASH_LEN]")]
#[case::literal("[u8; 32]", &[], "[u8 ; 32]")]
#[case::expression("[u8; HASH_LEN * 2]", &[], "[u8 ; crate :: models :: file :: HASH_LEN * 2]")]
#[case::expression_two_consts("[u8; HASH_LEN + SHARED_LEN]", &[], "[u8 ; crate :: models :: file :: HASH_LEN + crate :: consts :: SHARED_LEN]")]
#[case::expression_block("[u8; { HASH_LEN + 1 }]", &[], "[u8 ; { crate :: models :: file :: HASH_LEN + 1 }]")]
#[case::expression_const_generic("[u8; N * 2]", &["N"], "[u8 ; N * 2]")]
#[case::expression_unknown_call("[u8; size_of::<u64>()]", &[], "[u8 ; size_of :: < u64 > ()]")]
#[case::const_generic("[u8; HASH_LEN]", &["HASH_LEN"], "[u8 ; HASH_LEN]")]
#[case::unknown("[u8; OTHER]", &[], "[u8 ; OTHER]")]
#[case::leading_colon("[u8; ::lib::LEN]", &[], "[u8 ; :: lib :: LEN]")]
// R20-S4-M1 (round 20): a leading `super` run resolves as one prefix
// (`super::super` from `crate::models::file` is `crate`).
#[case::double_super_type("super::super::consts::Thing", &[], "crate :: consts :: Thing")]
// R20-S4-L3 (round 20): a const generic argument written as a block is
// re-rooted like an array length.
#[case::const_generic_block(
    "Digest<{ HASH_LEN }>",
    &[],
    "crate :: models :: file :: Digest < { crate :: models :: file :: HASH_LEN } >"
)]
fn array_length_constant_is_rerooted(
    #[case] ty: &str,
    #[case] generic_params: &[&str],
    #[case] expected: &str,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("models")).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod models; pub mod consts;").unwrap();
    std::fs::write(src.join("consts.rs"), "pub const SHARED_LEN: usize = 4;").unwrap();
    std::fs::write(src.join("models.rs"), "pub mod file;").unwrap();
    std::fs::write(
        src.join("models").join("file.rs"),
        "use crate::consts::SHARED_LEN; pub const HASH_LEN: usize = 32;",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let ty: syn::Type = syn::parse_str(ty).unwrap();
    let module = ["crate", "models", "file"].map(String::from);
    let params: Vec<String> = generic_params.iter().map(ToString::to_string).collect();
    assert_eq!(
        resolve_type_with_params(&ty, &module, &params).to_string(),
        expected
    );
}

/// R34-SM4-L2: a `Decimal` reached through a crate-internal re-export of the
/// external crate keeps its ecosystem default; a crate-local `Decimal` does not.
#[rstest]
#[case::reexported_external("pub use rust_decimal::Decimal;", Some(serde_json::json!("0")))]
#[case::crate_local("pub struct Decimal(pub i64);", None)]
fn reexported_ecosystem_name_keeps_its_default(
    #[case] types: &str,
    #[case] expected: Option<serde_json::Value>,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod types; pub mod sibling;").unwrap();
    std::fs::write(src.join("types.rs"), types).unwrap();
    std::fs::write(src.join("sibling.rs"), "use crate::types::Decimal;").unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let ty: syn::Type = syn::parse_str("Decimal").unwrap();
    let module = ["crate", "sibling"].map(String::from);
    assert_eq!(get_type_default(&ty, &module), expected);
}

/// H1 (round 15): a multi-segment type reached through a `::`-rooted import
/// (`use ::std::collections;` + `collections::HashMap<..>`) is rendered with
/// the leading `::` (the empty root segment panicked in `segment_ident`).
#[rstest]
#[case::rooted_module(
    "collections::HashMap<String, i32>",
    ":: std :: collections :: HashMap < String , i32 >"
)]
#[case::rooted_renamed(
    "coll::BTreeMap<String, u8>",
    ":: std :: collections :: BTreeMap < String , u8 >"
)]
#[case::rooted_single("Rc<u8>", ":: std :: rc :: Rc < u8 >")]
fn rooted_import_prefix_keeps_leading_colon(#[case] ty: &str, #[case] expected: &str) {
    let dir = tempfile::TempDir::new().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("lib.rs"), "pub mod m;").unwrap();
    std::fs::write(
        src.join("m.rs"),
        "use ::std::collections; use ::std::collections as coll; use ::std::rc::Rc;",
    )
    .unwrap();
    let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
    crate::schema_macro::file_cache::bump_epoch();
    let ty: syn::Type = syn::parse_str(ty).unwrap();
    let module = ["crate", "m"].map(String::from);
    assert_eq!(
        resolve_type_with_params(&ty, &module, &[]).to_string(),
        expected
    );
}
