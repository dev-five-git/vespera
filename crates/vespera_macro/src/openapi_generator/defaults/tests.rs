use std::collections::BTreeMap;

use rstest::rstest;
use serde_json::{Value, json};
use vespera_core::schema::{Reference, Schema, SchemaRef};

use super::*;

fn parse_expr(src: &str) -> syn::Expr {
    syn::parse_str(src).expect("expr parses")
}

fn parse_fn(src: &str) -> syn::ItemFn {
    syn::parse_str(src).expect("fn parses")
}

fn parse_type(src: &str) -> syn::Type {
    syn::parse_str(src).expect("type parses")
}

// ---------- extract_value_from_expr ----------

#[rstest]
#[case::int("42", Some(Value::Number(42.into())))]
#[case::string(r#""hello""#, Some(Value::String("hello".to_string())))]
#[case::bool_true("true", Some(Value::Bool(true)))]
#[case::bool_false("false", Some(Value::Bool(false)))]
#[case::to_string(r#""hello".to_string()"#, Some(Value::String("hello".to_string())))]
#[case::string_from(r#"String::from("hello")"#, Some(Value::String("hello".to_string())))]
#[case::vec_macro("vec![]", Some(Value::Array(vec![])))]
#[case::vec_macro_nonempty("vec![1, 2, 3]", Some(json!([1, 2, 3])))]
#[case::vec_macro_strings(r#"vec!["a", "b"]"#, Some(json!(["a", "b"])))]
#[case::vec_macro_unresolvable("vec![some_var]", None)]
#[case::int_to_string("42.to_string()", Some(Value::Number(42.into())))]
#[case::binary_unsupported("1 + 2", None)]
#[case::method_call_non_to_string(r#""hello".len()"#, None)]
#[case::byte_lit_unsupported("b'a'", None)]
#[case::non_vec_macro(r#"println!("test")"#, None)]
#[case::nested_paren_receiver(r#"("hello").to_string()"#, None)]
#[case::non_literal_receiver("some_var.to_string()", None)]
#[case::int_overflow("999999999999999999999999999999", None)]
fn extract_value_from_expr_cases(#[case] src: &str, #[case] expected: Option<Value>) {
    assert_eq!(extract_value_from_expr(&parse_expr(src)), expected);
}

#[test]
fn extract_value_from_expr_float_in_range() {
    // Float equality probe is separate — 12.34 round-trips but the assertion
    // needs a tolerance check rather than direct equality.
    let value = extract_value_from_expr(&parse_expr("12.34"));
    match value {
        Some(Value::Number(n)) => assert!((n.as_f64().unwrap() - 12.34).abs() < 0.001),
        other => panic!("expected number, got {other:?}"),
    }
}

#[test]
fn extract_value_from_expr_float_parse_failure_does_not_panic() {
    // 1e999999 may parse to infinity or fail — either way the call must not panic.
    let _ = extract_value_from_expr(&parse_expr("1e999999"));
}

// ---------- get_type_default (re-exported helper) ----------

#[rstest]
#[case::string("String", Some(Value::String(String::new())))]
#[case::i8("i8", Some(Value::Number(0.into())))]
#[case::i16("i16", Some(Value::Number(0.into())))]
#[case::i32("i32", Some(Value::Number(0.into())))]
#[case::i64("i64", Some(Value::Number(0.into())))]
#[case::u8("u8", Some(Value::Number(0.into())))]
#[case::u16("u16", Some(Value::Number(0.into())))]
#[case::u32("u32", Some(Value::Number(0.into())))]
#[case::u64("u64", Some(Value::Number(0.into())))]
#[case::bool("bool", Some(Value::Bool(false)))]
#[case::unknown_custom("CustomType", None)]
#[case::non_path_ref("&str", None)]
#[case::tuple("(i32, String)", None)]
#[case::array("[i32; 3]", None)]
fn get_type_default_cases(#[case] src: &str, #[case] expected: Option<Value>) {
    assert_eq!(utils_get_type_default(&parse_type(src)), expected);
}

#[rstest]
#[case::f32("f32")]
#[case::f64("f64")]
fn get_type_default_floats_present(#[case] src: &str) {
    assert!(utils_get_type_default(&parse_type(src)).is_some());
}

#[test]
fn get_type_default_global_path_still_resolved() {
    // `::String` has a leading colon-colon but the last segment is still `String`.
    assert!(utils_get_type_default(&parse_type("::String")).is_some());
}

// ---------- find_function_in_file ----------

#[rstest]
#[case("foo", true)]
#[case("defaults::foo", true)]
#[case("bar", true)]
#[case("baz", true)]
#[case("nonexistent", false)]
fn find_function_in_file_cases(#[case] needle: &str, #[case] expected: bool) {
    let file: syn::File = syn::parse_str(
        r"
        fn foo() {}
        fn bar() -> i32 { 42 }
        fn baz(x: i32) -> i32 { x }
        ",
    )
    .unwrap();
    assert_eq!(find_function_in_file(&file, needle).is_some(), expected);
}

// ---------- extract_default_value_from_function ----------

#[test]
fn extract_default_value_from_function_direct_expr() {
    let func = parse_fn("fn default_value() -> i32 { 42 }");
    assert_eq!(
        extract_default_value_from_function(&func),
        Some(Value::Number(42.into()))
    );
}

#[test]
fn extract_default_value_from_function_explicit_return() {
    let func = parse_fn(r#"fn default_value() -> String { return "hello".to_string() }"#);
    assert_eq!(
        extract_default_value_from_function(&func),
        Some(Value::String("hello".to_string()))
    );
}

#[test]
fn process_default_functions_applies_string_default_fn_value() {
    let file_ast: syn::File = syn::parse_str(
        r#"
        fn default_sort() -> String { "asc".to_string() }
        fn default_direction() -> String { String::from("desc") }
        "#,
    )
    .unwrap();
    let struct_item: syn::ItemStruct = syn::parse_str(
        r#"
        pub struct Test {
            #[serde(default = "default_sort")]
            pub sort: String,
            #[serde(default = "default_direction")]
            pub direction: String,
        }
        "#,
    )
    .unwrap();
    let mut schema = Schema::object();
    let props = schema.properties.get_or_insert_with(BTreeMap::new);
    props.insert(
        "sort".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    props.insert(
        "direction".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );

    process_default_functions(&struct_item, Some(&file_ast), &mut schema, &BTreeMap::new());

    let properties = schema.properties.as_ref().unwrap();
    assert_inline_default(properties, "sort", &json!("asc"));
    assert_inline_default(properties, "direction", &json!("desc"));
}

#[test]
fn process_default_functions_preserves_lexical_string_default() {
    // A `#[schema(default = "...")]` on a string field must keep the literal
    // verbatim — a numeric-looking default like a zero-padded id must NOT be
    // parsed to a number and back (which dropped leading zeroes:
    // "00123" -> "123").
    let struct_item: syn::ItemStruct = syn::parse_str(
        r#"
        pub struct Test {
            #[schema(default = "00123")]
            pub zip: String,
        }
        "#,
    )
    .unwrap();
    let mut schema = Schema::object();
    let props = schema.properties.get_or_insert_with(BTreeMap::new);
    props.insert(
        "zip".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );

    process_default_functions(&struct_item, None, &mut schema, &BTreeMap::new());

    let properties = schema.properties.as_ref().unwrap();
    assert_inline_default(properties, "zip", &json!("00123"));
}

#[test]
fn extract_default_value_from_function_no_value() {
    let func = parse_fn("fn default_value() { let x = 1; }");
    assert!(extract_default_value_from_function(&func).is_none());
}

// ---------- extract_schema_default_attr ----------

#[rstest]
#[case::with_value(
    syn::parse_quote!(#[schema(default = "42")]),
    Some("42".to_string()),
)]
#[case::no_default(syn::parse_quote!(#[schema(rename = "foo")]), None)]
#[case::non_schema(syn::parse_quote!(#[serde(default)]), None)]
fn extract_schema_default_attr_cases(
    #[case] attr: syn::Attribute,
    #[case] expected: Option<String>,
) {
    assert_eq!(extract_schema_default_attr(&[attr]), expected);
}

// ---------- parse_default_string_to_json_value ----------

#[rstest]
#[case::integer("42", json!(42))]
#[case::float("2.72", json!(2.72))]
#[case::bool("true", json!(true))]
#[case::string_fallback("hello world", json!("hello world"))]
fn parse_default_string_to_json_value_cases(#[case] input: &str, #[case] expected: Value) {
    assert_eq!(parse_default_string_to_json_value(input), expected);
}

// ---------- set_property_default ----------

fn inline_prop(default: Option<Value>) -> SchemaRef {
    let mut schema = Schema::object();
    schema.default = default;
    SchemaRef::Inline(Box::new(schema))
}

fn assert_inline_default(properties: &BTreeMap<String, SchemaRef>, key: &str, expected: &Value) {
    let SchemaRef::Inline(prop) = properties.get(key).expect("property present") else {
        panic!("expected inline schema for {key}");
    };
    assert_eq!(prop.default.as_ref(), Some(expected));
}

#[test]
fn set_property_default_sets_value_on_inline_schema_with_no_default() {
    let mut properties = BTreeMap::new();
    properties.insert("name".to_string(), inline_prop(None));

    set_property_default(&mut properties, "name", json!("Alice"));

    assert_inline_default(&properties, "name", &json!("Alice"));
}

#[test]
fn set_property_default_does_not_overwrite_existing() {
    let mut properties = BTreeMap::new();
    properties.insert("name".to_string(), inline_prop(Some(json!("existing"))));

    set_property_default(&mut properties, "name", json!("new"));

    assert_inline_default(&properties, "name", &json!("existing"));
}

#[test]
fn set_property_default_skips_ref_schema() {
    let mut properties = BTreeMap::new();
    properties.insert(
        "user".to_string(),
        SchemaRef::Ref(Reference::schema("User")),
    );

    set_property_default(&mut properties, "user", json!("ignored"));

    assert!(matches!(properties.get("user"), Some(SchemaRef::Ref(_))));
}

#[test]
fn set_property_default_skips_missing_property() {
    let mut properties = BTreeMap::new();

    set_property_default(&mut properties, "nonexistent", json!(42));

    assert!(properties.is_empty());
}

// ---------- process_default_functions ----------

#[test]
fn process_default_functions_early_returns_when_properties_none() {
    let struct_item: syn::ItemStruct = syn::parse_str("struct Empty;").unwrap();
    let file_ast: syn::File = syn::parse_str("fn foo() {}").unwrap();
    let mut schema = Schema::object();
    schema.properties = None;

    process_default_functions(&struct_item, Some(&file_ast), &mut schema, &BTreeMap::new());

    assert!(schema.properties.is_none());
}

#[test]
fn process_default_functions_applies_schema_default_attr() {
    let file_ast: syn::File = syn::parse_str("").unwrap();
    let struct_item: syn::ItemStruct =
        syn::parse_str(r#"pub struct Test { #[schema(default = "100")] pub count: i32 }"#).unwrap();
    let mut schema = Schema::object();
    let props = schema.properties.get_or_insert_with(BTreeMap::new);
    props.insert(
        "count".to_string(),
        SchemaRef::Inline(Box::new(Schema::integer())),
    );

    process_default_functions(&struct_item, Some(&file_ast), &mut schema, &BTreeMap::new());

    assert_inline_default(schema.properties.as_ref().unwrap(), "count", &json!(100));
}

#[test]
fn process_default_functions_applies_default_into_flatten_allof_member() {
    // Flatten struct: the own field `sort` (defaulted) lives in the inline
    // `allOf[0]` member, `pagination` is flattened to a `$ref`. The default
    // must still land on `sort` even though there is no top-level
    // `properties` map.
    let file_ast: syn::File =
        syn::parse_str(r#"fn default_sort() -> String { "asc".to_string() }"#).unwrap();
    let struct_item: syn::ItemStruct = syn::parse_str(
        r#"
        pub struct UserListRequest {
            #[serde(default = "default_sort")]
            pub sort: String,
            #[serde(flatten)]
            pub pagination: Pagination,
        }
        "#,
    )
    .unwrap();

    let mut inline = Schema::object();
    inline.properties.get_or_insert_with(BTreeMap::new).insert(
        "sort".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    let mut schema = Schema::default();
    schema.all_of = Some(vec![
        SchemaRef::Inline(Box::new(inline)),
        SchemaRef::Ref(Reference::schema("Pagination")),
    ]);

    process_default_functions(&struct_item, Some(&file_ast), &mut schema, &BTreeMap::new());

    let all_of = schema.all_of.as_ref().expect("allOf present");
    let SchemaRef::Inline(inline) = &all_of[0] else {
        panic!("expected inline allOf member");
    };
    assert_inline_default(inline.properties.as_ref().unwrap(), "sort", &json!("asc"));
}

#[test]
fn process_default_functions_demotes_unresolvable_fn_default_from_required() {
    // `#[serde(default = "fn")]` whose body is not a simple literal: no value
    // can be extracted at compile time, so the field must drop out of
    // `required` (a required field with no default is unsatisfiable).
    let file_ast: syn::File =
        syn::parse_str("fn complex() -> Vec<String> { compute_tags() }").unwrap();
    let struct_item: syn::ItemStruct = syn::parse_str(
        r#"
        pub struct Req {
            pub name: String,
            #[serde(default = "complex")]
            pub tags: Vec<String>,
        }
        "#,
    )
    .unwrap();
    let mut schema = Schema::object();
    let props = schema.properties.get_or_insert_with(BTreeMap::new);
    props.insert(
        "name".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    props.insert(
        "tags".to_string(),
        SchemaRef::Inline(Box::new(Schema::object())),
    );
    schema.required = Some(vec!["name".to_string(), "tags".to_string()]);

    process_default_functions(&struct_item, Some(&file_ast), &mut schema, &BTreeMap::new());

    let required = schema.required.as_ref().expect("required present");
    assert!(
        required.contains(&"name".to_string()),
        "name stays required"
    );
    assert!(
        !required.contains(&"tags".to_string()),
        "tags must be demoted: its serde default cannot be expressed"
    );
}

#[test]
fn process_default_functions_demotes_simple_default_without_type_default() {
    // `#[serde(default)]` on `Vec<T>`: `get_type_default` yields no value for
    // Vec, so the field is demoted from `required`.
    let struct_item: syn::ItemStruct = syn::parse_str(
        r"
        pub struct Req {
            pub name: String,
            #[serde(default)]
            pub tags: Vec<String>,
        }
        ",
    )
    .unwrap();
    let mut schema = Schema::object();
    let props = schema.properties.get_or_insert_with(BTreeMap::new);
    props.insert(
        "name".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    props.insert(
        "tags".to_string(),
        SchemaRef::Inline(Box::new(Schema::object())),
    );
    schema.required = Some(vec!["name".to_string(), "tags".to_string()]);

    process_default_functions(&struct_item, None, &mut schema, &BTreeMap::new());

    let required = schema.required.as_ref().expect("required present");
    assert!(required.contains(&"name".to_string()));
    assert!(
        !required.contains(&"tags".to_string()),
        "Vec serde default demoted"
    );
}

#[test]
fn process_default_functions_demotes_required_when_default_resolvable() {
    // A resolvable default still means the field is wire-optional: serde
    // accepts the payload with the key omitted and fills the value locally.
    let file_ast: syn::File =
        syn::parse_str(r#"fn default_sort() -> String { "asc".to_string() }"#).unwrap();
    let struct_item: syn::ItemStruct =
        syn::parse_str(r#"pub struct Req { #[serde(default = "default_sort")] pub sort: String }"#)
            .unwrap();
    let mut schema = Schema::object();
    schema.properties.get_or_insert_with(BTreeMap::new).insert(
        "sort".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    schema.required = Some(vec!["sort".to_string()]);

    process_default_functions(&struct_item, Some(&file_ast), &mut schema, &BTreeMap::new());

    assert!(schema.required.is_none(), "serde-default field is optional");
    assert_inline_default(schema.properties.as_ref().unwrap(), "sort", &json!("asc"));
}

// ---------- any_field_carries_default_relevant_attr ----------

#[rstest]
// No fields / no attrs: nothing to do, helper returns false.
#[case::no_fields("pub struct Empty;", false)]
#[case::tuple_struct("pub struct T(i32, String);", false)]
#[case::unit_struct("pub struct U;", false)]
#[case::plain_fields("pub struct P { pub a: i32, pub b: String }", false)]
// Non-serde/non-schema attrs: helper returns false, early-return fires.
#[case::doc_only("pub struct D { /// docs\n pub a: i32 }", false)]
#[case::garde_only(r"pub struct G { #[garde(length(min = 1))] pub a: String }", false)]
// Serde attrs of ANY kind (rename, default, skip, ...): return true.
#[case::serde_rename(r#"pub struct S { #[serde(rename = "a")] pub field: String }"#, true)]
#[case::serde_default_bare("pub struct S { #[serde(default)] pub field: String }", true)]
#[case::serde_default_fn(r#"pub struct S { #[serde(default = "f")] pub field: String }"#, true)]
// Schema attrs of ANY kind: return true.
#[case::schema_default(r#"pub struct S { #[schema(default = "1")] pub field: i32 }"#, true)]
// Mixed attr lists: any one match is enough.
#[case::serde_on_second_field(
    r#"pub struct M { pub a: i32, #[serde(rename = "x")] pub b: String }"#,
    true
)]
fn any_field_carries_default_relevant_attr_cases(#[case] src: &str, #[case] expected: bool) {
    let struct_item: syn::ItemStruct = syn::parse_str(src).expect("struct parses");
    assert_eq!(
        any_field_carries_default_relevant_attr(&struct_item),
        expected
    );
}

// ---------- early-return short-circuit ----------

#[test]
fn process_default_functions_early_returns_when_no_defaults_possible() {
    // The bench-fixture pattern: empty `stored_defaults`, no serde/schema
    // attrs anywhere. Every priority path is provably a no-op, so the
    // function must early-return WITHOUT mutating the schema and WITHOUT
    // demoting the field from `required`.
    let struct_item: syn::ItemStruct =
        syn::parse_str("pub struct Plain { pub id: i32, pub name: String }").unwrap();
    let mut schema = Schema::object();
    let props = schema.properties.get_or_insert_with(BTreeMap::new);
    props.insert(
        "id".to_string(),
        SchemaRef::Inline(Box::new(Schema::integer())),
    );
    props.insert(
        "name".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    schema.required = Some(vec!["id".to_string(), "name".to_string()]);

    process_default_functions(&struct_item, None, &mut schema, &BTreeMap::new());

    let properties = schema.properties.as_ref().unwrap();
    let SchemaRef::Inline(id) = properties.get("id").unwrap() else {
        panic!("expected inline");
    };
    let SchemaRef::Inline(name) = properties.get("name").unwrap() else {
        panic!("expected inline");
    };
    assert!(id.default.is_none(), "no default must be set on `id`");
    assert!(name.default.is_none(), "no default must be set on `name`");
    // `required` must be unchanged — no serde-default field exists.
    assert_eq!(
        schema.required.as_deref(),
        Some(["id".to_string(), "name".to_string()].as_slice())
    );
}

#[test]
fn process_default_functions_falls_through_for_serde_rename_only() {
    // `#[serde(rename = ...)]` is not a default attribute, but the helper
    // intentionally treats ANY `serde` attr as relevant — so the full walk
    // must still run (no early-return). The OUTCOME is the same as the
    // early-return path (no defaults set, `required` unchanged) because
    // `rename` is not a default, but we lock that behaviour here so a
    // future change to the helper (e.g. narrowing to `default`-only attrs)
    // does not silently flip semantics.
    let struct_item: syn::ItemStruct = syn::parse_str(
        r#"
        pub struct R {
            #[serde(rename = "n")]
            pub name: String,
        }
        "#,
    )
    .unwrap();
    let mut schema = Schema::object();
    let props = schema.properties.get_or_insert_with(BTreeMap::new);
    props.insert(
        "n".to_string(),
        SchemaRef::Inline(Box::new(Schema::string())),
    );
    schema.required = Some(vec!["n".to_string()]);

    process_default_functions(&struct_item, None, &mut schema, &BTreeMap::new());

    let properties = schema.properties.as_ref().unwrap();
    let SchemaRef::Inline(n) = properties.get("n").unwrap() else {
        panic!("expected inline");
    };
    assert!(n.default.is_none(), "rename-only does not set a default");
    assert_eq!(
        schema.required.as_deref(),
        Some(["n".to_string()].as_slice())
    );
}

#[test]
fn process_default_functions_falls_through_when_stored_defaults_present() {
    // `stored_defaults` populated by `#[derive(Schema)]`: even without any
    // field-level attrs the early-return MUST NOT fire — the Priority-0
    // path must apply the stored default.
    let struct_item: syn::ItemStruct =
        syn::parse_str("pub struct Plain { pub count: i32 }").unwrap();
    let mut schema = Schema::object();
    schema.properties.get_or_insert_with(BTreeMap::new).insert(
        "count".to_string(),
        SchemaRef::Inline(Box::new(Schema::integer())),
    );
    let stored: BTreeMap<String, Value> = BTreeMap::from([("count".to_string(), json!(7))]);

    process_default_functions(&struct_item, None, &mut schema, &stored);

    assert_inline_default(schema.properties.as_ref().unwrap(), "count", &json!(7));
}

#[test]
fn process_default_functions_falls_through_for_schema_default_attr() {
    // `#[schema(default = "...")]` with empty `stored_defaults`: the
    // early-return MUST NOT fire because Priority-1 needs to run.
    let struct_item: syn::ItemStruct = syn::parse_str(
        r#"
        pub struct S {
            #[schema(default = "42")]
            pub count: i32,
        }
        "#,
    )
    .unwrap();
    let mut schema = Schema::object();
    schema.properties.get_or_insert_with(BTreeMap::new).insert(
        "count".to_string(),
        SchemaRef::Inline(Box::new(Schema::integer())),
    );

    process_default_functions(&struct_item, None, &mut schema, &BTreeMap::new());

    assert_inline_default(schema.properties.as_ref().unwrap(), "count", &json!(42));
}
