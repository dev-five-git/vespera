//! Unit tests for `type_schema::conversion` (sidecar module; `use super::*`
//! semantics preserved).

use super::*;

fn empty_known() -> HashSet<String> {
    HashSet::new()
}

fn empty_struct_definitions() -> HashMap<String, String> {
    HashMap::new()
}

// ========== Coverage: generic known schema edge cases ==========

#[test]
fn test_generic_known_schema_no_struct_definition() {
    // Known schema with angle brackets but NO struct_definitions entry → falls through to Ref
    let mut known = HashSet::new();
    known.insert("Wrapper".to_string());
    // Do NOT insert into struct_definitions
    let ty: Type = syn::parse_str("Wrapper<String>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &empty_struct_definitions());
    // Should fall through to non-generic ref path
    assert!(
        matches!(schema_ref, SchemaRef::Ref(_)),
        "Should be a $ref when no struct definition found"
    );
}

#[test]
fn test_generic_known_schema_param_count_mismatch() {
    // Struct has 1 generic param but 2 concrete types provided → falls through to Ref
    let mut known = HashSet::new();
    known.insert("Single".to_string());
    let mut defs = HashMap::new();
    defs.insert(
        "Single".to_string(),
        "struct Single<T> { value: T }".to_string(),
    );

    let ty: Type = syn::parse_str("Single<String, i32>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
    assert!(
        matches!(schema_ref, SchemaRef::Ref(_)),
        "Mismatched param count should fall through to $ref"
    );
}

#[test]
fn test_generic_known_schema_invalid_definition() {
    // struct_definitions has invalid Rust code → parse fails → falls through to Ref
    let mut known = HashSet::new();
    known.insert("Bad".to_string());
    let mut defs = HashMap::new();
    defs.insert("Bad".to_string(), "not valid rust code!!!".to_string());

    let ty: Type = syn::parse_str("Bad<String>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
    assert!(
        matches!(schema_ref, SchemaRef::Ref(_)),
        "Invalid definition should fall through to $ref"
    );
}

#[test]
fn test_generic_known_schema_tuple_struct() {
    // Tuple struct fields are NOT Named → skips field substitution but still inlines
    let mut known = HashSet::new();
    known.insert("Pair".to_string());
    let mut defs = HashMap::new();
    defs.insert("Pair".to_string(), "struct Pair<T>(T, T);".to_string());

    let ty: Type = syn::parse_str("Pair<String>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
    // Tuple struct still gets inlined (generics cleared, parse_struct_to_schema called)
    // but field types are NOT substituted (no Named fields to iterate)
    assert!(
        matches!(schema_ref, SchemaRef::Inline(_)),
        "Tuple struct should still inline"
    );
}

#[test]
fn test_generic_known_schema_no_generic_params_in_def() {
    // Struct definition has no generics but concrete type has angle brackets → mismatch
    let mut known = HashSet::new();
    known.insert("Plain".to_string());
    let mut defs = HashMap::new();
    defs.insert("Plain".to_string(), "struct Plain { x: i32 }".to_string());

    let ty: Type = syn::parse_str("Plain<String>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
    // 0 generic params != 1 concrete type → falls through to Ref
    assert!(matches!(schema_ref, SchemaRef::Ref(_)));
}

// ========== Coverage: nested generic types ==========

#[test]
fn test_nested_vec_vec_string() {
    let ty: Type = syn::parse_str("Vec<Vec<String>>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Array));
        if let Some(SchemaRef::Inline(inner)) = schema.items.as_ref() {
            assert_eq!(inner.schema_type, Some(SchemaType::Array));
            if let Some(SchemaRef::Inline(innermost)) = inner.items.as_ref() {
                assert_eq!(innermost.schema_type, Some(SchemaType::String));
            } else {
                panic!("Expected innermost inline schema");
            }
        } else {
            panic!("Expected inner inline schema");
        }
    } else {
        panic!("Expected inline schema for nested Vec");
    }
}

#[test]
fn test_option_vec_i32() {
    let ty: Type = syn::parse_str("Option<Vec<i32>>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Array));
        assert_eq!(schema.nullable, Some(true));
        if let Some(SchemaRef::Inline(items)) = schema.items.as_ref() {
            assert_eq!(items.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline items");
        }
    } else {
        panic!("Expected inline schema for Option<Vec<i32>>");
    }
}

#[test]
fn test_box_box_i32() {
    // Box<Box<i32>> → transparent twice → integer
    let ty: Type = syn::parse_str("Box<Box<i32>>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Integer));
    } else {
        panic!("Expected inline integer schema for Box<Box<i32>>");
    }
}

// ========== Coverage: HashMap/BTreeMap with known ref value ==========

#[test]
fn test_hashmap_with_known_ref_value() {
    let mut known = HashSet::new();
    known.insert("User".to_string());
    let ty: Type = syn::parse_str("HashMap<String, User>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Object));
        let additional = schema.additional_properties.as_ref().unwrap();
        let AdditionalProperties::Schema(SchemaRef::Ref(reference)) = additional else {
            panic!("expected a schema-ref additionalProperties, got {additional:?}");
        };
        assert_eq!(reference.ref_path, "#/components/schemas/User");
    } else {
        panic!("Expected inline schema for HashMap<String, User>");
    }
}

#[test]
fn test_btreemap_with_inline_value() {
    let ty: Type = syn::parse_str("BTreeMap<String, Vec<i32>>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Object));
        let additional = schema.additional_properties.as_ref().unwrap();
        // Value should be an inline array schema.
        let AdditionalProperties::Schema(SchemaRef::Inline(value_schema)) = additional else {
            panic!("expected an inline-schema additionalProperties, got {additional:?}");
        };
        assert_eq!(value_schema.schema_type, Some(SchemaType::Array));
    } else {
        panic!("Expected inline schema for BTreeMap with Vec value");
    }
}

// ========== Coverage: HashMap/BTreeMap with insufficient args ==========

#[test]
fn test_hashmap_single_arg_falls_through() {
    // HashMap<String> — only 1 type arg, need 2 → falls through to unknown type
    let ty: Type = syn::parse_str("HashMap<String>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, None, "unknown shape: any value");
        // Should NOT have additional_properties since it fell through
        assert!(schema.additional_properties.is_none());
    } else {
        panic!("Expected inline schema");
    }
}

// ========== Coverage: &mut T reference ==========

#[test]
fn test_mutable_reference_delegates_to_inner() {
    let ty: Type = syn::parse_str("&mut String").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::String));
    } else {
        panic!("Expected inline string schema for &mut String");
    }
}

// ========== Coverage: HashSet/BTreeSet → uniqueItems ==========

#[test]
fn test_hashset_string_produces_unique_items_array() {
    let ty: Type = syn::parse_str("HashSet<String>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Array));
        assert_eq!(schema.unique_items, Some(true));
        if let Some(SchemaRef::Inline(items)) = schema.items.as_ref() {
            assert_eq!(items.schema_type, Some(SchemaType::String));
        } else {
            panic!("Expected inline string items for HashSet<String>");
        }
    } else {
        panic!("Expected inline schema for HashSet<String>");
    }
}

#[test]
fn test_btreeset_i32_produces_unique_items_array() {
    let ty: Type = syn::parse_str("BTreeSet<i32>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Array));
        assert_eq!(schema.unique_items, Some(true));
        if let Some(SchemaRef::Inline(items)) = schema.items.as_ref() {
            assert_eq!(items.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline integer items for BTreeSet<i32>");
        }
    } else {
        panic!("Expected inline schema for BTreeSet<i32>");
    }
}

#[test]
fn test_option_hashset_is_nullable_unique_array() {
    let ty: Type = syn::parse_str("Option<HashSet<i64>>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Array));
        assert_eq!(schema.unique_items, Some(true));
        assert_eq!(schema.nullable, Some(true));
        if let Some(SchemaRef::Inline(items)) = schema.items.as_ref() {
            assert_eq!(items.schema_type, Some(SchemaType::Integer));
        } else {
            panic!("Expected inline integer items for Option<HashSet<i64>>");
        }
    } else {
        panic!("Expected inline schema for Option<HashSet<i64>>");
    }
}

#[test]
fn test_vec_does_not_have_unique_items() {
    let ty: Type = syn::parse_str("Vec<String>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    if let SchemaRef::Inline(schema) = &schema_ref {
        assert_eq!(schema.schema_type, Some(SchemaType::Array));
        assert!(schema.unique_items.is_none());
    } else {
        panic!("Expected inline schema for Vec<String>");
    }
}

#[test]
fn test_bare_hashset_without_generics() {
    // HashSet without angle brackets → falls through to bare-name match
    let ty: Type = syn::parse_str("HashSet").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    assert!(matches!(schema_ref, SchemaRef::Inline(_)));
}

#[test]
fn test_bare_btreeset_without_generics() {
    let ty: Type = syn::parse_str("BTreeSet").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    assert!(matches!(schema_ref, SchemaRef::Inline(_)));
}

#[test]
fn test_known_schema_ref_override_returns_inline_ref_schema() {
    let mut known = HashSet::new();
    known.insert("UserSchema".to_string());

    let mut defs = HashMap::new();
    defs.insert(
        "UserSchema".to_string(),
        r#"
        #[schema(ref = "ExternalUser", nullable)]
        struct UserSchema {
            id: i32,
        }
        "#
        .to_string(),
    );

    let ty: Type = syn::parse_str("UserSchema").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);

    match schema_ref {
        SchemaRef::Inline(schema) => {
            assert_eq!(
                schema.ref_path.as_deref(),
                Some("#/components/schemas/ExternalUser")
            );
            assert_eq!(schema.nullable, Some(true));
        }
        SchemaRef::Ref(_) => panic!("expected inline schema ref override"),
    }
}

#[rstest::rstest]
#[case::u128("u128")]
#[case::usize("usize")]
fn unsigned_wide_integer_has_zero_minimum(#[case] source: &str) {
    let ty: Type = syn::parse_str(source).expect("type");
    let SchemaRef::Inline(schema) =
        parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions())
    else {
        panic!("inline schema");
    };
    assert_eq!(schema.schema_type, Some(SchemaType::Integer));
    assert_eq!(schema.minimum, Some(0.0));
}

#[test]
fn generic_definition_ignores_lifetime_parameters() {
    let known = HashSet::from(["Borrowed".to_string()]);
    let defs = HashMap::from([(
        "Borrowed".to_string(),
        "struct Borrowed<'a, T> { value: T, marker: &'a str }".to_string(),
    )]);
    let ty: Type = syn::parse_str("Borrowed<'static, String>").expect("type");

    let schema = parse_type_to_schema_ref(&ty, &known, &defs);

    let SchemaRef::Inline(schema) = schema else {
        panic!("matching type arguments should inline");
    };
    assert_eq!(schema.schema_type, Some(SchemaType::Object));
}

#[test]
fn lifetime_only_concrete_argument_falls_back_to_reference() {
    let known = HashSet::from(["Borrowed".to_string()]);
    let defs = HashMap::from([(
        "Borrowed".to_string(),
        "struct Borrowed<T> { value: T }".to_string(),
    )]);
    let ty: Type = syn::parse_str("Borrowed<'static>").expect("type");

    assert!(matches!(
        parse_type_to_schema_ref(&ty, &known, &defs),
        SchemaRef::Ref(_)
    ));
}

#[test]
fn unit_type_maps_to_null_schema() {
    let ty: Type = syn::parse_quote!(());
    let SchemaRef::Inline(schema) =
        parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions())
    else {
        panic!("inline schema");
    };
    assert_eq!(schema.schema_type, Some(SchemaType::Null));
}

// ========== Smart pointers, std collections, NonZero, tuples, arrays ==========

/// Every new shape, rendered as compact JSON for one readable snapshot each.
#[rstest::rstest]
#[case::arc("arc", "Arc<i32>")]
#[case::rc("rc", "std::rc::Rc<String>")]
#[case::cow_str("cow_str", "Cow<'a, str>")]
#[case::cow_known("cow_known", "Cow<'static, User>")]
#[case::vec_deque("vec_deque", "VecDeque<bool>")]
#[case::linked_list("linked_list", "LinkedList<u8>")]
#[case::non_zero_u32("non_zero_u32", "NonZeroU32")]
#[case::non_zero_usize("non_zero_usize", "std::num::NonZeroUsize")]
#[case::non_zero_i64("non_zero_i64", "NonZeroI64")]
#[case::non_zero_generic_u16("non_zero_generic_u16", "NonZero<u16>")]
#[case::non_zero_generic_i8("non_zero_generic_i8", "NonZero<i8>")]
#[case::tuple_pair("tuple_pair", "(i32, String)")]
#[case::tuple_single("tuple_single", "(bool,)")]
#[case::array_fixed("array_fixed", "[f32; 3]")]
#[case::array_const_len("array_const_len", "[u8; N]")]
#[case::slice_ref("slice_ref", "&[User]")]
#[case::paren("paren", "(i64)")]
fn extended_type_shapes(#[case] name: &str, #[case] ty_src: &str) {
    let known: HashSet<String> = HashSet::from(["User".to_string()]);
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &empty_struct_definitions());
    let json = serde_json::to_string(&schema_ref).unwrap();
    insta::assert_snapshot!(
        format!("extended_type_shape_{name}"),
        format!("{ty_src} => {json}")
    );
}

#[rstest::rstest]
#[case("Arc")]
#[case("Rc")]
#[case("Cow")]
#[case("VecDeque")]
#[case("LinkedList")]
#[case("NonZero")]
#[case("NonZeroU8")]
#[case("NonZeroI128")]
#[case("NonZeroUsize")]
fn new_wrappers_are_builtin_openapi_types(#[case] ident: &str) {
    assert!(is_builtin_openapi_type(ident));
    // Bare (argument-less) wrappers fall back to an inline schema, never a $ref.
    let ty: Type = syn::parse_str(ident).unwrap();
    assert!(matches!(
        parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions()),
        SchemaRef::Inline(_)
    ));
}

#[test]
fn generic_tuple_struct_substitutes_type_params() {
    let known: HashSet<String> = HashSet::from(["Pair".to_string()]);
    let defs: HashMap<String, String> =
        HashMap::from([("Pair".to_string(), "struct Pair<T>(T, T);".to_string())]);
    let ty: Type = syn::parse_str("Pair<String>").unwrap();
    let SchemaRef::Inline(schema) = parse_type_to_schema_ref(&ty, &known, &defs) else {
        panic!("generic tuple struct is inlined");
    };
    let items = schema.prefix_items.expect("tuple struct → prefixItems");
    assert_eq!(items.len(), 2);
    for item in items {
        let SchemaRef::Inline(item) = item else {
            panic!("inline")
        };
        assert_eq!(item.schema_type, Some(SchemaType::String));
    }
}

/// `macro_rules!`-expanded `$t:ty` fragments arrive as invisible
/// `Type::Group`s and must map exactly like the bare type.
#[test]
fn invisible_type_group_maps_like_its_contents() {
    let known: HashSet<String> = HashSet::from(["User".to_string()]);
    for inner in [
        quote::quote!(i64),
        quote::quote!(Vec<User>),
        quote::quote!(Option<String>),
    ] {
        let bare: Type = syn::parse2(inner.clone()).unwrap();
        let grouped: Type = syn::parse2(
            proc_macro2::TokenTree::Group(proc_macro2::Group::new(
                proc_macro2::Delimiter::None,
                inner.clone(),
            ))
            .into(),
        )
        .unwrap();
        assert!(matches!(grouped, Type::Group(_)));
        assert_eq!(
            serde_json::to_value(parse_type_to_schema_ref(
                &grouped,
                &known,
                &empty_struct_definitions()
            ))
            .unwrap(),
            serde_json::to_value(parse_type_to_schema_ref(
                &bare,
                &known,
                &empty_struct_definitions()
            ))
            .unwrap(),
            "{inner}"
        );
    }
}

/// Types with no known JSON shape (fn pointers, `impl Trait`, trait
/// objects, unknown types, unresolved generic parameters, bare wrappers)
/// fall back to any value (`{}`), never `object`.
#[rstest::rstest]
#[case::fn_pointer("fn(i32) -> i32")]
#[case::impl_trait("impl Iterator<Item = u8>")]
#[case::trait_object("dyn std::any::Any")]
#[case::unknown_type("Unknown")]
#[case::generic_param("T")]
#[case::qualified_unknown("other::Thing")]
#[case::bare_wrapper("Vec")]
#[case::option_unknown("Option<T>")]
fn shapeless_types_are_any_value(#[case] ty_src: &str) {
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    assert!(
        matches!(schema_ref, SchemaRef::Inline(_)),
        "{ty_src} must not become a $ref"
    );
    assert_eq!(
        serde_json::to_string(&schema_ref).unwrap(),
        "{}",
        "{ty_src}"
    );
}

// ========== String-serialized std::net / std::path / url types ==========

/// serde writes every one of these as a JSON string; the IP families and
/// `Url` carry their registered OpenAPI formats.
#[rstest::rstest]
#[case::ipv4("ipv4", "Ipv4Addr")]
#[case::ipv6("ipv6", "std::net::Ipv6Addr")]
#[case::ip_addr("ip_addr", "IpAddr")]
#[case::socket_addr("socket_addr", "std::net::SocketAddr")]
#[case::socket_addr_v4("socket_addr_v4", "SocketAddrV4")]
#[case::socket_addr_v6("socket_addr_v6", "SocketAddrV6")]
#[case::path_buf("path_buf", "std::path::PathBuf")]
#[case::url("url", "url::Url")]
#[case::option_url("option_url", "Option<Url>")]
#[case::vec_ip("vec_ip", "Vec<IpAddr>")]
fn string_serialized_std_types(#[case] name: &str, #[case] ty_src: &str) {
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let last = match &ty {
        Type::Path(type_path) => type_path.path.segments.last().unwrap().ident.to_string(),
        _ => unreachable!("every case is a path type"),
    };
    assert!(is_builtin_openapi_type(&last));
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    let json = serde_json::to_string(&schema_ref).unwrap();
    insta::assert_snapshot!(
        format!("string_serialized_std_type_{name}"),
        format!("{ty_src} => {json}")
    );
}

/// A user `Schema` type that happens to share a builtin name (`Url`,
/// `IpAddr`, `Date`, `Decimal`, `StatusCode`, …) keeps its own `$ref`
/// instead of the builtin mapping — also through a crate-local path.
#[rstest::rstest]
#[case("Url", "Url")]
#[case("IpAddr", "IpAddr")]
#[case("PathBuf", "PathBuf")]
#[case("Date", "Date")]
#[case("Time", "Time")]
#[case("DateTime", "DateTime")]
#[case("Decimal", "Decimal")]
#[case("Uuid", "Uuid")]
#[case("Duration", "Duration")]
#[case("StatusCode", "StatusCode")]
#[case("Header", "Header")]
#[case("Path", "Path")]
#[case("Query", "Query")]
#[case("Json", "Json")]
#[case("Result", "Result")]
#[case("Date", "crate::models::Date")]
fn user_schema_named_like_builtin_keeps_ref(#[case] ident: &str, #[case] ty_src: &str) {
    let known: HashSet<String> = HashSet::from([ident.to_string()]);
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &empty_struct_definitions());
    let SchemaRef::Ref(reference) = schema_ref else {
        panic!("expected $ref for a known `{ty_src}` schema, got {schema_ref:?}");
    };
    assert_eq!(reference.ref_path, format!("#/components/schemas/{ident}"));
}

/// Qualified with the external crate, the builtin mapping still applies even
/// when a same-named user schema exists.
#[rstest::rstest]
#[case("Date", "chrono::NaiveDate", r#"{"type":"string","format":"date"}"#)]
#[case(
    "Decimal",
    "rust_decimal::Decimal",
    r#"{"type":"string","format":"decimal"}"#
)]
#[case("Uuid", "uuid::Uuid", r#"{"type":"string","format":"uuid"}"#)]
#[case("StatusCode", "http::StatusCode", r#"{"type":"integer"}"#)]
#[case("Url", "url::Url", r#"{"type":"string","format":"uri"}"#)]
fn external_crate_path_keeps_builtin_mapping(
    #[case] ident: &str,
    #[case] ty_src: &str,
    #[case] expected: &str,
) {
    let known: HashSet<String> = HashSet::from([ident.to_string(), "NaiveDate".to_string()]);
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &empty_struct_definitions());
    assert_eq!(serde_json::to_string(&schema_ref).unwrap(), expected);
}

/// RFC 3339 `date-time` / `time` require a UTC offset: only offset-carrying
/// types use them; naive date-times / times are plain strings.
#[rstest::rstest]
#[case::chrono_utc("chrono::DateTime<Utc>", Some("date-time"))]
#[case::chrono_fixed("DateTime<FixedOffset>", Some("date-time"))]
#[case::sea_orm_tz("DateTimeWithTimeZone", Some("date-time"))]
#[case::sea_orm_utc("DateTimeUtc", Some("date-time"))]
#[case::sea_orm_local("DateTimeLocal", Some("date-time"))]
#[case::time_offset("time::OffsetDateTime", None)]
#[case::chrono_naive("chrono::NaiveDateTime", None)]
#[case::sea_orm_naive_alias("sea_orm::prelude::DateTime", None)]
#[case::time_primitive("time::PrimitiveDateTime", None)]
#[case::chrono_naive_time("NaiveTime", None)]
#[case::time_time("time::Time", None)]
#[case::date("NaiveDate", Some("date"))]
#[case::sea_orm_date_alias("Date", Some("date"))]
#[case::time_date("time::Date", None)]
fn date_time_formats_require_an_offset(#[case] ty_src: &str, #[case] format: Option<&str>) {
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let SchemaRef::Inline(schema) =
        parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions())
    else {
        panic!("inline schema for {ty_src}");
    };
    assert_eq!(schema.schema_type, Some(SchemaType::String), "{ty_src}");
    assert_eq!(schema.format.as_deref(), format, "{ty_src}");
}

/// The `time` crate does not write RFC 3339 (`2024-01-01 0:00:00.0
/// +00:00:00`), and writes a tuple without `serde-human-readable`: its types
/// are plain strings that say so. Bare `Date` / `Time` are sea-orm's chrono
/// aliases and keep their chrono schema.
#[rstest::rstest]
#[case::offset("OffsetDateTime", true)]
#[case::qualified_offset("time::OffsetDateTime", true)]
#[case::primitive("PrimitiveDateTime", true)]
#[case::time_date("time::Date", true)]
#[case::time_time("time::macros::Time", true)]
#[case::bare_date("Date", false)]
#[case::bare_time("Time", false)]
#[case::chrono_date("chrono::NaiveDate", false)]
fn time_crate_types_are_described(#[case] ty_src: &str, #[case] described: bool) {
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let SchemaRef::Inline(schema) =
        parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions())
    else {
        panic!("inline schema for {ty_src}");
    };
    assert_eq!(schema.schema_type, Some(SchemaType::String), "{ty_src}");
    assert_eq!(
        schema.description.as_deref() == Some(TIME_CRATE_DESCRIPTION),
        described,
        "{ty_src}"
    );
    if described {
        assert_eq!(schema.format, None, "{ty_src}");
    }
}

/// `Option<()>` is `null` either way: the type is a single `"null"`.
#[test]
fn option_unit_is_single_null_type() {
    let ty: Type = syn::parse_str("Option<()>").unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions());
    assert_eq!(
        serde_json::to_string(&schema_ref).unwrap(),
        r#"{"type":"null"}"#
    );
}

/// Every primitive arm of the wire-type table: `(type, json type, format,
/// minimum, maximum)`. Unsigned widths carry their value range (`u64::MAX`
/// is not exactly representable as a double, so `u64` has no `maximum`).
#[rstest::rstest]
#[case::i8("i8", SchemaType::Integer, Some("int8"), None, None)]
#[case::i16("i16", SchemaType::Integer, Some("int16"), None, None)]
#[case::non_zero_i16("NonZeroI16", SchemaType::Integer, Some("int16"), None, None)]
#[case::i32("i32", SchemaType::Integer, Some("int32"), None, None)]
#[case::i64("i64", SchemaType::Integer, Some("int64"), None, None)]
#[case::u8("u8", SchemaType::Integer, Some("uint8"), Some(0.0), Some(255.0))]
#[case::u16("u16", SchemaType::Integer, Some("uint16"), Some(0.0), Some(65_535.0))]
#[case::u32(
    "u32",
    SchemaType::Integer,
    Some("uint32"),
    Some(0.0),
    Some(4_294_967_295.0)
)]
#[case::u64("u64", SchemaType::Integer, Some("uint64"), Some(0.0), None)]
#[case::f32("f32", SchemaType::Number, Some("float"), None, None)]
#[case::f64("f64", SchemaType::Number, Some("double"), None, None)]
#[case::decimal("Decimal", SchemaType::String, Some("decimal"), None, None)]
#[case::char("char", SchemaType::String, Some("char"), None, None)]
#[case::uuid("Uuid", SchemaType::String, Some("uuid"), None, None)]
#[case::non_zero_u8(
    "NonZeroU8",
    SchemaType::Integer,
    Some("uint8"),
    Some(1.0),
    Some(255.0)
)]
#[case::non_zero_u16(
    "NonZeroU16",
    SchemaType::Integer,
    Some("uint16"),
    Some(1.0),
    Some(65_535.0)
)]
#[case::non_zero_u32(
    "NonZeroU32",
    SchemaType::Integer,
    Some("uint32"),
    Some(1.0),
    Some(4_294_967_295.0)
)]
#[case::non_zero_u64("NonZeroU64", SchemaType::Integer, Some("uint64"), Some(1.0), None)]
#[case::non_zero_u128("NonZeroU128", SchemaType::Integer, None, Some(1.0), None)]
fn primitive_wire_types(
    #[case] source: &str,
    #[case] schema_type: SchemaType,
    #[case] format: Option<&str>,
    #[case] minimum: Option<f64>,
    #[case] maximum: Option<f64>,
) {
    let ty: Type = syn::parse_str(source).expect("type");
    let SchemaRef::Inline(schema) =
        parse_type_to_schema_ref(&ty, &empty_known(), &empty_struct_definitions())
    else {
        panic!("inline schema for {source}");
    };
    assert_eq!(schema.schema_type, Some(schema_type), "{source}");
    assert_eq!(schema.format.as_deref(), format, "{source}");
    assert_eq!(schema.minimum, minimum, "{source}");
    assert_eq!(schema.maximum, maximum, "{source}");
}

/// A known definition that is neither a struct nor an enum (a type alias)
/// cannot be substituted: the generic use falls back to a `$ref`.
#[test]
fn generic_non_struct_definition_falls_back_to_reference() {
    let known: HashSet<String> = HashSet::from(["Alias".to_string()]);
    let defs: HashMap<String, String> =
        HashMap::from([("Alias".to_string(), "type Alias<T> = Vec<T>;".to_string())]);
    let ty: Type = syn::parse_str("Alias<i32>").unwrap();
    let SchemaRef::Ref(reference) = parse_type_to_schema_ref(&ty, &known, &defs) else {
        panic!("expected $ref");
    };
    assert_eq!(reference.ref_path, "#/components/schemas/Alias");
}

/// A concrete generic ENUM is inlined with its type arguments substituted in
/// every variant, exactly like a generic struct (it used to become a `$ref`
/// to the unsubstituted component, documenting `T` as an object).
#[rstest::rstest]
#[case::externally_tagged(
    "generic_enum_external",
    "ApiResponse",
    "enum ApiResponse<T> { Ok(T), Err { message: String } }",
    "ApiResponse<Vec<User>>"
)]
#[case::internally_tagged(
    "generic_enum_internal",
    "Event",
    r#"#[serde(tag = "type")] enum Event<P> { Created { payload: P }, Deleted }"#,
    "Event<i32>"
)]
#[case::two_params(
    "generic_enum_two_params",
    "Either",
    "enum Either<L, R> { Left(L), Right(R) }",
    "Either<String, Option<User>>"
)]
fn generic_enum_substitutes_type_params(
    #[case] snapshot: &str,
    #[case] base: &str,
    #[case] definition: &str,
    #[case] ty_src: &str,
) {
    let known: HashSet<String> = HashSet::from([base.to_string(), "User".to_string()]);
    let defs: HashMap<String, String> = HashMap::from([(base.to_string(), definition.to_string())]);
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
    assert!(
        matches!(schema_ref, SchemaRef::Inline(_)),
        "generic enum is inlined"
    );
    insta::assert_snapshot!(
        snapshot,
        format!(
            "{ty_src} => {}",
            serde_json::to_string_pretty(&schema_ref).unwrap()
        )
    );
}

/// L7: a type argument left out takes the parameter's default, substituted
/// over the earlier parameters (`B = Vec<A>`).
#[rstest::rstest]
#[case::default_struct(
    "generic_default_struct",
    "struct Page<T, M = Meta> { items: Vec<T>, meta: M }",
    "Page<User>"
)]
#[case::default_mentions_earlier(
    "generic_default_mentions_earlier",
    "struct Pair<A, B = Vec<A>> { a: A, b: B }",
    "Pair<u8>"
)]
fn generic_defaults_are_substituted(
    #[case] snapshot: &str,
    #[case] definition: &str,
    #[case] ty_src: &str,
) {
    let item: syn::ItemStruct = syn::parse_str(definition).unwrap();
    let base = item.ident.to_string();
    let known: HashSet<String> = HashSet::from([base.clone(), "User".into(), "Meta".into()]);
    let defs: HashMap<String, String> = HashMap::from([(base, definition.to_string())]);
    let ty: Type = syn::parse_str(ty_src).unwrap();
    let schema_ref = parse_type_to_schema_ref(&ty, &known, &defs);
    insta::assert_snapshot!(
        snapshot,
        format!(
            "{ty_src} => {}",
            serde_json::to_string(&schema_ref).unwrap()
        )
    );
}

/// A missing argument without a default cannot be substituted: `$ref`.
#[test]
fn generic_missing_argument_without_default_is_a_reference() {
    let known: HashSet<String> = HashSet::from(["Pair".to_string()]);
    let defs = HashMap::from([(
        "Pair".to_string(),
        "struct Pair<A, B> { a: A, b: B }".to_string(),
    )]);
    let ty: Type = syn::parse_str("Pair<u8>").unwrap();
    assert!(matches!(
        parse_type_to_schema_ref(&ty, &known, &defs),
        SchemaRef::Ref(_)
    ));
}

/// A recursive generic instance whose argument reads differently than it
/// writes (a set: `uniqueItems` on output only) is a separate `...Input`
/// component in the request view.
#[test]
fn recursive_generic_instance_over_a_set_has_an_input_name() {
    use super::super::generic_instances::take_materialized;
    let known: HashSet<String> = HashSet::from(["Tree".to_string()]);
    let defs = HashMap::from([(
        "Tree".to_string(),
        "struct Tree<T> { v: T, kids: Vec<Tree<T>> }".to_string(),
    )]);
    let ty: Type = syn::parse_str("Tree<HashSet<u8>>").unwrap();
    let response = parse_type_to_schema_ref(&ty, &known, &defs);
    let response_names: Vec<String> = take_materialized().into_keys().collect();
    let request = crate::parser::schema::request_view::with_request_view(|| {
        parse_type_to_schema_ref(&ty, &known, &defs)
    });
    let request_names: Vec<String> = take_materialized().into_keys().collect();
    assert_eq!(response_names, ["Tree_HashSet_u8"]);
    assert_eq!(request_names, ["Tree_HashSet_u8Input"]);
    let (SchemaRef::Ref(response), SchemaRef::Ref(request)) = (response, request) else {
        panic!("a recursive instance is a $ref");
    };
    assert_eq!(response.ref_path, "#/components/schemas/Tree_HashSet_u8");
    assert_eq!(
        request.ref_path,
        "#/components/schemas/Tree_HashSet_u8Input"
    );
}
