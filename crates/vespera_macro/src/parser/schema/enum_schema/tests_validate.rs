//! `validate_enum_for_schema`: which internally tagged newtype payloads are
//! rejected at `#[derive(Schema)]` time.

use rstest::rstest;

use super::*;

/// L5: a payload converted by a custom serde codec (field- or variant-level
/// `with` / `serialize_with` / `deserialize_with`) has an unknown wire shape,
/// so its declared type proves nothing; `#[schema(value_type = T)]` documents
/// the payload as `T`, which is what gets classified.
#[rstest]
#[case::field_with(r#"Data(#[serde(with = "codec")] String)"#, false)]
#[case::field_serialize_with(r#"Data(#[serde(serialize_with = "f")] Vec<u8>)"#, false)]
#[case::field_deserialize_with(r#"Data(#[serde(deserialize_with = "f")] u64)"#, false)]
#[case::variant_with(r#"#[serde(with = "codec")] Data(String)"#, false)]
#[case::value_type_object("Data(#[schema(value_type = Payload)] String)", false)]
#[case::value_type_non_object("Data(#[schema(value_type = String)] Payload)", true)]
#[case::other_attribute(r#"Data(#[serde(rename = "x")] String)"#, true)]
fn codec_and_value_type_payloads(#[case] variant: &str, #[case] is_error: bool) {
    let enum_item: syn::ItemEnum = syn::parse_str(&format!(
        r#"#[serde(tag = "type")] enum Event {{ Ping, {variant} }}"#
    ))
    .unwrap();
    assert_eq!(
        validate_enum_for_schema(&enum_item).is_err(),
        is_error,
        "{variant}"
    );
}

/// Only syntactically unambiguous non-object payloads are rejected for an
/// internally tagged newtype variant; path types that may be user structs
/// (or map-serialized std types like `Duration`) are accepted.
#[rstest]
#[case::string("String", true)]
#[case::integer("u64", true)]
#[case::str_ref("&'static str", true)]
#[case::std_string("std::string::String", true)]
#[case::vec("Vec<i32>", true)]
#[case::set("std::collections::BTreeSet<u8>", true)]
#[case::array("[u8; 4]", true)]
#[case::tuple("(i32, i32)", true)]
#[case::option_primitive("Option<bool>", true)]
#[case::boxed_string("Box<String>", true)]
#[case::unit("()", false)]
#[case::user_struct("Payload", false)]
#[case::uuid("Uuid", false)]
#[case::date("Date", false)]
#[case::time("chrono::NaiveTime", false)]
#[case::std_duration("std::time::Duration", false)]
#[case::decimal("rust_decimal::Decimal", false)]
#[case::user_vec("my::Vec<i32>", false)]
#[case::option_struct("Option<Payload>", false)]
#[case::map("std::collections::HashMap<String, i32>", false)]
#[case::parenthesized("(String)", true)]
#[case::option_str_ref("Option<&'static str>", true)]
#[case::option_parenthesized("Option<(u8)>", true)]
#[case::option_array("Option<[u8; 2]>", false)]
#[case::qualified_self("<T as Trait>::Output", false)]
#[case::fn_pointer("fn() -> i32", false)]
// `Cow` is not a wrapper the classifier looks through (a `Cow<T>` may be an
// object), so it is left to serde.
#[case::cow_is_not_classified("std::borrow::Cow<'static, str>", false)]
fn internally_tagged_newtype_payload_classification(#[case] inner: &str, #[case] is_error: bool) {
    let inner: syn::Type = syn::parse_str(inner).unwrap();
    let enum_item: syn::ItemEnum = syn::parse_quote! {
        #[serde(tag = "type")]
        enum Event { Ping, Data(#inner) }
    };
    assert_eq!(
        validate_enum_for_schema(&enum_item).is_err(),
        is_error,
        "{}",
        quote::quote!(#inner)
    );
}

/// `macro_rules!`-generated enums carry `$t:ty` payloads as invisible
/// `Type::Group`s; classification must look through them.
#[test]
fn invisible_type_groups_are_classified_by_their_contents() {
    let group = |elem: syn::Type| -> syn::Type {
        let tokens = proc_macro2::Group::new(proc_macro2::Delimiter::None, quote::quote!(#elem));
        let ty = syn::parse2(proc_macro2::TokenTree::Group(tokens).into()).unwrap();
        assert!(
            matches!(ty, syn::Type::Group(_)),
            "None-delimited group parses as Type::Group"
        );
        ty
    };
    assert!(is_unambiguous_non_object(&group(syn::parse_quote!(String))));
    assert!(!is_unambiguous_non_object(&group(syn::parse_quote!(
        Payload
    ))));
    let option_of_group: syn::Type = {
        let inner = group(syn::parse_quote!(u32));
        syn::parse_quote!(Option<#inner>)
    };
    assert!(is_unambiguous_non_object(&option_of_group));
}

/// Multi-field tuple variants are a serde compile error under
/// `#[serde(tag = ...)]`; the schema check leaves that diagnosis to serde.
#[test]
fn internally_tagged_multi_field_tuple_variant_is_left_to_serde() {
    let enum_item: syn::ItemEnum = syn::parse_quote! {
        #[serde(tag = "type")]
        enum Event { Ping, Pair(String, String) }
    };
    assert!(validate_enum_for_schema(&enum_item).is_ok());
}

/// M1: a container documented as another type on both sides (`value_type`,
/// serde `into` + `from` / `try_from`) never uses the tagged layout, so its
/// newtype payloads are not checked; one overridden side still is.
#[rstest]
#[case::value_type("#[schema(value_type = String)]", false)]
#[case::into_try_from(r#"#[serde(into = "String", try_from = "String")]"#, false)]
#[case::into_from(r#"#[serde(into = "String", from = "String")]"#, false)]
#[case::into_only(r#"#[serde(into = "String")]"#, true)]
#[case::from_only(r#"#[serde(from = "String")]"#, true)]
fn container_override_skips_the_tagged_layout(#[case] attrs: &str, #[case] is_error: bool) {
    let enum_item: syn::ItemEnum = syn::parse_str(&format!(
        r#"{attrs} #[serde(tag = "t")] enum E {{ A(u8) }}"#
    ))
    .unwrap();
    assert_eq!(
        validate_enum_for_schema(&enum_item).is_err(),
        is_error,
        "{attrs}"
    );
}

/// serde itself accepts the M1 enum: the tag never reaches the wire.
#[test]
fn serde_converts_an_overridden_internally_tagged_enum() {
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", into = "String", try_from = "String")]
    enum E {
        A(u8),
    }
    impl From<E> for String {
        fn from(E::A(n): E) -> Self {
            n.to_string()
        }
    }
    impl TryFrom<String> for E {
        type Error = std::num::ParseIntError;
        fn try_from(s: String) -> Result<Self, Self::Error> {
            s.parse().map(Self::A)
        }
    }
    assert_eq!(serde_json::to_string(&E::A(3)).unwrap(), r#""3""#);
    assert!(matches!(serde_json::from_str::<E>(r#""4""#), Ok(E::A(4))));
}
