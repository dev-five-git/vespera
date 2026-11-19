use super::*;
use quote::quote;
use syn::parse_quote;

fn parse_ty(src: &str) -> Type {
    syn::parse_str::<Type>(src).expect("valid type")
}

fn leaves_of(src: &str) -> Vec<String> {
    collect_leaf_custom_types(&parse_ty(src))
        .iter()
        .map(|(ty, _)| quote!(#ty).to_string().replace(' ', ""))
        .collect()
}

fn empty_path_type() -> Type {
    Type::Path(syn::TypePath {
        attrs: Vec::new(),
        qself: None,
        path: syn::Path {
            leading_colon: None,
            segments: syn::punctuated::Punctuated::new(),
        },
    })
}

/// Recompute the per-field `SchemaConstraints` slice the way
/// `process_derive_schema` does in production code, so tests can
/// drive `emit_field_assertions` (which now takes the slice as a
/// parameter) without re-stating the parse pass at every call site.
fn constraints_for(input: &syn::DeriveInput) -> Vec<SchemaConstraints> {
    use crate::parser::schema::schema_attrs::try_extract_schema_constraints;
    if let syn::Data::Struct(d) = &input.data
        && let syn::Fields::Named(f) = &d.fields
    {
        return f
            .named
            .iter()
            .map(|fld| try_extract_schema_constraints(&fld.attrs).unwrap_or_default())
            .collect();
    }
    Vec::new()
}

// ── collect_leaf_custom_types ──────────────────────────────────

#[test]
fn leaf_of_plain_path_is_itself() {
    assert_eq!(leaves_of("MyType"), vec!["MyType"]);
}

#[test]
fn leaf_of_vec_unwraps_inner() {
    assert_eq!(leaves_of("Vec<MyType>"), vec!["MyType"]);
}

#[test]
fn leaf_of_option_unwraps_inner() {
    assert_eq!(leaves_of("Option<MyType>"), vec!["MyType"]);
}

#[test]
fn leaf_of_box_unwraps_inner() {
    assert_eq!(leaves_of("Box<MyType>"), vec!["MyType"]);
}

#[test]
fn leaf_of_hashset_unwraps_inner() {
    assert_eq!(leaves_of("HashSet<MyType>"), vec!["MyType"]);
}

#[test]
fn leaf_of_btreeset_unwraps_inner() {
    assert_eq!(leaves_of("BTreeSet<MyType>"), vec!["MyType"]);
}

/// Smart pointers / extra std collections are transparent like `Box`/`Vec`.
#[rstest::rstest]
#[case::arc("Arc<MyType>")]
#[case::rc("std::rc::Rc<MyType>")]
#[case::cow("Cow<'static, MyType>")]
#[case::vec_deque("VecDeque<MyType>")]
#[case::linked_list("LinkedList<MyType>")]
#[case::nested("Arc<Vec<Cow<'a, MyType>>>")]
fn leaf_of_smart_pointers_and_collections_unwraps_inner(#[case] src: &str) {
    assert_eq!(leaves_of(src), vec!["MyType"]);
}

#[rstest::rstest]
#[case::non_zero_u32("NonZeroU32")]
#[case::non_zero_generic("NonZero<u64>")]
#[case::cow_str("Cow<'a, str>")]
#[case::arc_string("Arc<String>")]
#[case::ip_addr("std::net::IpAddr")]
#[case::ipv4("Ipv4Addr")]
#[case::ipv6("Option<Ipv6Addr>")]
#[case::socket_addr("Vec<SocketAddr>")]
#[case::socket_addr_v4("SocketAddrV4")]
#[case::socket_addr_v6("SocketAddrV6")]
#[case::path_buf("std::path::PathBuf")]
#[case::url("Option<url::Url>")]
#[case::duration("std::time::Duration")]
fn builtin_wrapped_leaves_need_no_assertion(#[case] src: &str) {
    for (_, ident) in collect_leaf_custom_types(&parse_ty(src)) {
        assert!(is_builtin_openapi_type(&ident), "{src}: {ident}");
    }
}

#[test]
fn leaf_of_hashmap_unwraps_value_not_key() {
    // Key is always String by JSON convention; only the value gets
    // an OpenAPI schema reference, so only the value gets asserted.
    assert_eq!(leaves_of("HashMap<String, MyType>"), vec!["MyType"]);
}

#[test]
fn leaf_of_btreemap_unwraps_value_not_key() {
    assert_eq!(leaves_of("BTreeMap<String, MyType>"), vec!["MyType"]);
}

#[test]
fn leaf_of_nested_wrappers() {
    assert_eq!(
        leaves_of("Vec<Option<Box<HashMap<String, MyType>>>>"),
        vec!["MyType"]
    );
}

/// References, slices, arrays, tuples, parentheses and `$t:ty` groups
/// are documented through their elements, so their custom leaves are
/// asserted too.
#[rstest::rstest]
#[case::tuple("(MyType, u32)", &["MyType", "u32"])]
#[case::reference("&'a MyType", &["MyType"])]
#[case::slice("&[MyType]", &["MyType"])]
#[case::array("[Vec<MyType>; 2]", &["MyType"])]
#[case::paren("(MyType)", &["MyType"])]
#[case::unit("()", &[])]
#[case::fn_pointer("fn(MyType)", &[])]
fn leaf_of_structural_types(#[case] src: &str, #[case] expected: &[&str]) {
    assert_eq!(leaves_of(src), expected);
}

#[test]
fn leaf_of_macro_type_group() {
    let group = Type::Group(syn::TypeGroup {
        attrs: Vec::new(),
        group_token: syn::token::Group::default(),
        elem: Box::new(parse_ty("Option<MyType>")),
    });
    let leaves: Vec<String> = collect_leaf_custom_types(&group)
        .into_iter()
        .map(|(_, ident)| ident)
        .collect();
    assert_eq!(leaves, ["MyType"]);
}

#[test]
fn leaf_of_empty_wrapper_is_empty() {
    // `Vec` with no type args (degenerate) has no leaf.
    assert_eq!(
        collect_leaf_custom_types(&parse_ty("Vec")),
        Vec::<(Type, String)>::new()
    );
}

#[test]
fn leaf_of_empty_path_is_empty() {
    assert_eq!(
        collect_leaf_custom_types(&empty_path_type()),
        Vec::<(Type, String)>::new()
    );
}

#[test]
fn generic_argument_helpers_ignore_lifetimes_and_missing_arguments() {
    let vec_lifetime = parse_ty("Vec<'a>");
    let Type::Path(vec_path) = vec_lifetime else {
        panic!("fixture must be a path type");
    };
    let vec_segment = vec_path.path.segments.last().expect("Vec segment");

    let bare_map = parse_ty("HashMap");
    let Type::Path(bare_map_path) = bare_map else {
        panic!("fixture must be a path type");
    };
    let bare_map_segment = bare_map_path.path.segments.last().expect("HashMap segment");

    let lifetime_map = parse_ty("HashMap<'a, 'b>");
    let Type::Path(lifetime_map_path) = lifetime_map else {
        panic!("fixture must be a path type");
    };
    let lifetime_map_segment = lifetime_map_path
        .path
        .segments
        .last()
        .expect("HashMap segment");

    assert_eq!(first_generic_type_arg(vec_segment), None);
    assert_eq!(second_generic_type_arg(bare_map_segment), None);
    assert_eq!(second_generic_type_arg(lifetime_map_segment), None);
}

#[test]
fn leaf_pairs_carry_the_final_path_ident() {
    let leaves = collect_leaf_custom_types(&parse_ty("Vec<Option<crate::models::User>>"));
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].1, "User");
}

// ── is_serde_json_value_leaf ────────────────────────────────────

#[test]
fn bare_value_is_allowlisted() {
    assert!(is_serde_json_value_leaf(&parse_ty("Value")));
}

#[test]
fn serde_json_value_is_allowlisted() {
    assert!(is_serde_json_value_leaf(&parse_ty("serde_json::Value")));
}

#[test]
fn leading_colon_serde_json_value_is_allowlisted() {
    assert!(is_serde_json_value_leaf(&parse_ty("::serde_json::Value")));
}

#[test]
fn vespera_reexport_serde_json_value_is_allowlisted() {
    assert!(is_serde_json_value_leaf(&parse_ty(
        "vespera::serde_json::Value"
    )));
}

#[test]
fn unrelated_value_segment_is_not_allowlisted() {
    // `foo::bar::Value` is some user enum named Value — not the
    // serde_json sentinel, so the strict-schema check still fires.
    assert!(!is_serde_json_value_leaf(&parse_ty("foo::bar::Value")));
}

#[test]
fn last_segment_not_value_is_not_allowlisted() {
    assert!(!is_serde_json_value_leaf(&parse_ty("serde_json::Map")));
}

#[test]
fn serde_json_allowlist_rejects_non_path_and_empty_path_types() {
    assert!(!is_serde_json_value_leaf(&parse_ty("&Value")));
    assert!(!is_serde_json_value_leaf(&empty_path_type()));
}

// ── type_contains_generic ───────────────────────────────────────

#[test]
fn generic_param_directly() {
    let mut generics = HashSet::new();
    generics.insert("T".to_string());
    assert!(type_contains_generic(&parse_ty("T"), &generics));
}

#[test]
fn generic_param_inside_vec() {
    let mut generics = HashSet::new();
    generics.insert("T".to_string());
    assert!(type_contains_generic(&parse_ty("Vec<T>"), &generics));
}

/// A generic named like a later segment of a qualified path does not
/// shadow the path (`models::User` under `struct Page<User>` is still
/// asserted), while a generic leading a path (`T::Item`) is skipped.
#[test]
fn qualified_path_is_not_shadowed_by_a_later_generic_segment() {
    let mut generics = HashSet::new();
    generics.insert("User".to_string());
    assert!(!type_contains_generic(&parse_ty("models::User"), &generics));
    assert!(!type_contains_generic(
        &parse_ty("::models::User"),
        &generics
    ));
    assert!(type_contains_generic(&parse_ty("Vec<User>"), &generics));
    assert!(type_contains_generic(&parse_ty("User::Item"), &generics));
    assert!(type_contains_generic(&parse_ty("Self"), &generics));
}

#[test]
fn generic_param_inside_box_inside_option() {
    let mut generics = HashSet::new();
    generics.insert("T".to_string());
    assert!(type_contains_generic(
        &parse_ty("Option<Box<HashMap<String, T>>>"),
        &generics
    ));
}

#[test]
fn no_generic_param_present() {
    let mut generics = HashSet::new();
    generics.insert("T".to_string());
    assert!(!type_contains_generic(&parse_ty("Vec<String>"), &generics));
    assert!(!type_contains_generic(
        &parse_ty("HashMap<String, MyType>"),
        &generics
    ));
}

#[test]
fn empty_generics_set_never_matches() {
    let generics = HashSet::new();
    assert!(!type_contains_generic(&parse_ty("T"), &generics));
}

#[test]
fn generic_detection_descends_through_references_and_rejects_other_types() {
    let generics = HashSet::from(["T".to_string()]);

    assert!(type_contains_generic(&parse_ty("&T"), &generics));
    assert!(type_contains_generic(&parse_ty("(T, String)"), &generics));
    assert!(!type_contains_generic(&parse_ty("&MyType"), &generics));
    assert!(!type_contains_generic(&parse_ty("(u8, String)"), &generics));
}

// ── emit_marker_impl preserves generics ─────────────────────────

#[test]
fn marker_impl_for_plain_struct() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Plain { x: i32 }
    };
    let out = emit_marker_impl(&input).to_string();
    assert!(out.contains("impl :: vespera :: Schema for Plain"));
}

#[test]
fn marker_impl_preserves_generics_and_bounds() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Container<T: Serialize> { value: T }
    };
    let out = emit_marker_impl(&input).to_string();
    assert!(out.contains("impl < T : Serialize > :: vespera :: Schema for Container < T >"));
}

#[test]
fn marker_impl_for_enum() {
    let input: syn::DeriveInput = parse_quote! {
        pub enum E { A, B(i32) }
    };
    let out = emit_marker_impl(&input).to_string();
    assert!(out.contains("impl :: vespera :: Schema for E"));
}

// ── emit_field_assertions exemption surface ─────────────────────

#[test]
fn no_assertion_for_builtin_only_struct() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Plain {
            id: i32,
            name: String,
            tags: Vec<String>,
            map: HashMap<String, bool>,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(
        out.is_empty(),
        "builtins-only struct must emit zero assertions, got: {out}"
    );
}

#[test]
fn assertion_for_unknown_custom_leaf() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Holder {
            inner: MyUnknown,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(
        out.contains("__vespera_assert_schema") && out.contains("MyUnknown"),
        "expected assertion on MyUnknown, got: {out}"
    );
}

#[test]
fn no_assertion_for_serde_json_value() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Holder {
            raw: serde_json::Value,
            bare: Value,
            via: Option<Vec<serde_json::Value>>,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(
        out.is_empty(),
        "Value fields must not emit assertions, got: {out}"
    );
}

#[test]
fn no_assertion_for_phantom_data_of_a_user_type() {
    // Non-generic struct: `PhantomData<User>` has no `Schema` impl, so an
    // assertion on it would fail to compile (round-27 macrorest-4 M1).
    let input: syn::DeriveInput = parse_quote! {
        pub struct UserId {
            pub raw: u64,
            pub _marker: PhantomData<User>,
            pub other: std::marker::PhantomData<Account>,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(
        out.is_empty(),
        "PhantomData fields must emit no assertions, got: {out}"
    );
}

#[test]
fn phantom_data_of_a_user_module_path_is_still_asserted() {
    // `api::PhantomData` is a user's own type, not `core::marker::PhantomData`.
    let input: syn::DeriveInput = parse_quote! {
        pub struct Holder {
            inner: api::PhantomData,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(
        out.contains("__vespera_assert_schema"),
        "expected assertion on api::PhantomData, got: {out}"
    );
}

#[test]
fn no_assertion_for_any_marked_field() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Holder {
            #[schema(any)]
            opaque: MyUnknown,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(
        out.is_empty(),
        "`#[schema(any)]` must skip the assertion, got: {out}"
    );
}

#[test]
fn no_assertion_for_generic_parameter_leaf() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Generic<T> {
            value: T,
            vec: Vec<T>,
            map: HashMap<String, T>,
            name: String,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(
        out.is_empty(),
        "fields whose leaf is the struct's generic param must not emit assertions, got: {out}"
    );
}

/// Leaves no module-level `const _` can name (a lifetime / const
/// parameter, `Self`) and fully skipped fields emit nothing.
#[rstest::rstest]
#[case::serde_skip(parse_quote!(struct S { #[serde(skip)] pool: PgPool }))]
#[case::skip_both_sides(parse_quote!(struct S {
    #[serde(skip_serializing)]
    #[serde(skip_deserializing)]
    pool: PgPool,
}))]
#[case::lifetime(parse_quote!(struct S<'a> { name: Name<'a> }))]
#[case::const_param(parse_quote!(struct S<const N: usize> { items: Items<N> }))]
#[case::self_type(parse_quote!(struct Node { children: Vec<Box<Self>> }))]
fn no_assertion_for_unnameable_or_skipped_leaf(#[case] input: syn::DeriveInput) {
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(out.is_empty(), "expected no assertion, got: {out}");
}

#[test]
fn one_sided_skip_still_asserts() {
    let input: syn::DeriveInput = parse_quote! {
        struct S<'a> { #[serde(skip_serializing)] pool: PgPool, other: Name<'static> }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    assert!(out.contains("PgPool") && out.contains("Name"), "{out}");
}

#[test]
fn assertion_per_unknown_field_individually() {
    let input: syn::DeriveInput = parse_quote! {
        pub struct Holder {
            a: AlphaType,
            b: Vec<BetaType>,
            c: HashMap<String, GammaType>,
            d: i32,
            #[schema(any)]
            e: WhateverType,
        }
    };
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    // Three assertions (a, b, c) for the three non-exempt leaves.
    assert_eq!(
        out.matches("let _ = __vespera_assert_schema").count(),
        3,
        "expected one assertion per non-exempt field: {out}"
    );
    assert!(out.contains("AlphaType"));
    assert!(out.contains("BetaType"));
    assert!(out.contains("GammaType"));
    assert!(!out.contains("WhateverType"));
}

#[test]
fn no_assertion_for_non_struct_kinds() {
    // Enums get the marker impl (separately) but the per-variant
    // field walk is intentionally out of scope for v1; their
    // variant fields fall back to silent `Object` as before.
    let input: syn::DeriveInput = parse_quote! {
        pub enum E { A(MyUnknown) }
    };
    assert_eq!(
        emit_field_assertions(&input, &constraints_for(&input)).to_string(),
        ""
    );

    let input: syn::DeriveInput = parse_quote! {
        pub struct Tuple(MyUnknown);
    };
    assert_eq!(
        emit_field_assertions(&input, &constraints_for(&input)).to_string(),
        ""
    );

    let input: syn::DeriveInput = parse_quote! {
        pub struct Unit;
    };
    assert_eq!(
        emit_field_assertions(&input, &constraints_for(&input)).to_string(),
        ""
    );
}

/// The leaf types asserted by `emit_field_assertions`, in emission order.
fn asserted_leaves(src: &str) -> Vec<String> {
    let input: syn::DeriveInput = syn::parse_str(src).unwrap();
    let out = emit_field_assertions(&input, &constraints_for(&input)).to_string();
    out.split("let _ = __vespera_assert_schema :: < ")
        .skip(1)
        .map(|rest| rest.split(" > ;").next().unwrap().replace(' ', ""))
        .collect()
}

#[rstest::rstest]
#[case::json_value_aliases(
    "struct S { a: JsonValue, b: sea_orm::JsonValue, c: Option<Json>, d: sea_orm::prelude::Json }",
    &[]
)]
#[case::field_value_type_replaces_field_type(
    "struct S { #[schema(value_type = Vec<Wire>)] a: Opaque, #[schema(value_type = i64)] b: Opaque }",
    &["Wire"]
)]
#[case::with_without_value_type_is_skipped(
    r#"struct S { #[serde(with = "m")] a: Opaque, #[serde(serialize_with = "f", deserialize_with = "g")] b: Opaque }"#,
    &[]
)]
#[case::one_sided_codec_keeps_the_field_type(
    r#"struct S { #[serde(serialize_with = "f")] a: Opaque }"#,
    &["Opaque"]
)]
#[case::container_into_keeps_fields(
    r#"#[serde(into = "Wire")] struct S { a: Opaque }"#,
    &["Wire", "Opaque"]
)]
#[case::container_both_sides_skip_fields(
    r#"#[serde(into = "Wire", try_from = "Raw")] struct S { a: Opaque }"#,
    &["Wire", "Raw"]
)]
#[case::container_value_type_skips_fields(
    "#[schema(value_type = Vec<Wire>)] struct S { a: Opaque }",
    &["Wire", "Wire"]
)]
#[case::enum_conversion(
    r#"#[serde(into = "String", from = "Raw")] enum E { A(Opaque) }"#,
    &["Raw"]
)]
#[case::generic_conversion_is_skipped(r#"#[serde(into = "Vec<T>")] struct S<T> { a: T }"#, &[])]
fn wire_override_assertions(#[case] src: &str, #[case] expected: &[&str]) {
    let _externs = with_externs(&LINKED_CRATES);
    assert_eq!(asserted_leaves(src), expected);
}

/// Registers `crates` as the `--extern` crates of the compilation under test.
/// A unit-test binary links none, so the compiler-args seam supplies them.
fn with_externs(crates: &[&str]) -> crate::env_access::test_env::EnvGuard {
    let args = std::iter::once("rustc".to_owned())
        .chain(crates.iter().flat_map(|name| {
            [
                "--extern".to_owned(),
                format!("{name}=/deps/lib{name}.rlib"),
            ]
        }))
        .collect::<Vec<_>>()
        .join("\n");
    crate::env_access::test_env::set_var(crate::env_access::COMPILER_ARGS_KEY, args)
}

/// The crates the builtin-named cases below are written against.
const LINKED_CRATES: [&str; 9] = [
    "chrono",
    "axum",
    "rust_decimal",
    "vespera",
    "sea_orm",
    "serde_json",
    "url",
    "http",
    "tempfile",
];

/// A builtin-named type under a user module is checked, so a forgotten
/// derive there cannot document as the builtin. Bare names and the paths
/// into a crate the compilation links (or into std) keep their builtin
/// exemption; a root that is neither is the user's.
#[rstest::rstest]
#[case::user_module_builtin_name("struct S { a: api::Header }", &["api::Header"])]
#[case::crate_path_builtin_name("struct S { a: crate::models::Date }", &["crate::models::Date"])]
#[case::unlinked_crate_builtin_name("struct S { a: unlinked::Url }", &["unlinked::Url"])]
#[case::dependency_builtin_names(
    "struct S { a: url::Url, b: http::StatusCode, c: tempfile::NamedTempFile }",
    &[]
)]
#[case::external_builtin_names(
    "struct S { a: chrono::DateTime<Utc>, b: axum::Json<u8>, c: rust_decimal::Decimal, d: vespera::Json<u8> }",
    &[]
)]
#[case::std_path_builtin_names("struct S { a: std::path::PathBuf, b: std::time::Duration }", &[])]
#[case::bare_builtin_names("struct S { a: Header, b: Decimal }", &[])]
#[case::user_json_value_alias("struct S { a: models::JsonValue }", &["models::JsonValue"])]
#[case::external_json_value_aliases("struct S { a: sea_orm::JsonValue, b: serde_json::Value }", &[])]
fn builtin_named_leaves_follow_their_path(#[case] src: &str, #[case] expected: &[&str]) {
    let _externs = with_externs(&LINKED_CRATES);
    assert_eq!(asserted_leaves(src), expected);
}

/// The origin decision itself: `true` only for a user-module path.
#[rstest::rstest]
#[case::user_module("api::Header", true)]
#[case::crate_root("crate::models::Date", true)]
#[case::self_module("self::Date", true)]
#[case::super_module("super::Uuid", true)]
#[case::unlinked_root("unlinked::Url", true)]
#[case::linked_dependency("url::Url", false)]
#[case::linked_leading_colon("::http::StatusCode", false)]
#[case::std_root("std::path::PathBuf", false)]
#[case::core_root("core::time::Duration", false)]
#[case::alloc_root("alloc::string::String", false)]
#[case::bare_name("Header", false)]
#[case::array_is_not_a_path("[u8; 2]", false)]
fn user_module_path_is_decided_by_origin(#[case] src: &str, #[case] user: bool) {
    let _externs = with_externs(&LINKED_CRATES);
    assert_eq!(is_user_module_path(&parse_ty(src)), user, "{src}");
}
