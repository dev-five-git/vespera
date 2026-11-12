//! `principal` request wire-header field: hand-rolled parser vs the serde
//! twin (accept/reject + value identity), edge errors, the entry cap, and the
//! owned [`crate::Principal`] conversion.

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::bench_serde::parse_wire_header_serde;
use super::{MAX_PRINCIPAL_ENTRIES, WirePrincipal, parse_wire_header};

/// Comparable projection of a parsed `principal` (borrow vs owned ignored).
type OwnedPrincipal = Option<(String, Vec<String>, Vec<(String, String)>)>;

fn owned(p: Option<&WirePrincipal<'_>>) -> OwnedPrincipal {
    p.map(|p| {
        (
            p.name.to_string(),
            p.authorities.iter().map(ToString::to_string).collect(),
            p.attributes
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    })
}

fn header(principal: &str) -> Vec<u8> {
    format!(r#"{{"v":1,"method":"GET","path":"/p","principal":{principal}}}"#).into_bytes()
}

/// Accepted forms: hand and serde agree on every value.
#[rstest::rstest]
#[case::null("null")]
#[case::name_only(r#"{"name":"alice"}"#)]
#[case::full(r#"{"name":"alice","authorities":["ROLE_A","ROLE_B"],"attributes":{"t":"acme"}}"#)]
#[case::any_key_order(r#"{"attributes":{},"authorities":[],"name":"a"}"#)]
#[case::whitespace(
    r#" { "name" : "a" , "authorities" : [ "x" , "y" ] , "attributes" : { "k" : "v" } } "#
)]
#[case::escapes(r#"{"name":"a\"bé😀","authorities":["\n"],"attributes":{"A":"\\"}}"#)]
#[case::unknown_keys_skipped(r#"{"x":[1,{"y":null}],"name":"a","z":true}"#)]
#[case::duplicate_attribute_keys_kept(r#"{"name":"a","attributes":{"k":"1","k":"2"}}"#)]
#[case::empty_name(r#"{"name":""}"#)]
fn hand_and_serde_accept_identically(#[case] principal: &str) {
    let input = header(principal);
    let hand = parse_wire_header(&input).expect("hand accepts");
    let serde = parse_wire_header_serde(&input).expect("serde accepts");
    assert_eq!(
        owned(hand.principal.as_ref()),
        owned(serde.principal.as_ref())
    );
}

/// Rejected forms: both reject, and the hand parser names the problem.
#[rstest::rstest]
#[case::missing_name(r"{}", "missing field `name` in `principal`")]
#[case::missing_name_with_keys(r#"{"authorities":[]}"#, "missing field `name` in `principal`")]
#[case::duplicate_name(r#"{"name":"a","name":"b"}"#, "duplicate field `name`")]
#[case::duplicate_authorities(
    r#"{"name":"a","authorities":[],"authorities":[]}"#,
    "duplicate field `authorities`"
)]
#[case::duplicate_attributes(
    r#"{"name":"a","attributes":{},"attributes":{}}"#,
    "duplicate field `attributes`"
)]
#[case::not_an_object("7", "expected '{'")]
#[case::array_form(r#"["a"]"#, "expected '{'")]
#[case::bad_null("nul", "invalid literal")]
#[case::name_not_string(r#"{"name":1}"#, "expected string")]
#[case::name_null(r#"{"name":null}"#, "expected string")]
#[case::authorities_not_array(r#"{"name":"a","authorities":"x"}"#, "expected '['")]
#[case::authority_not_string(r#"{"name":"a","authorities":[1]}"#, "expected string")]
#[case::authorities_trailing_comma(r#"{"name":"a","authorities":["x",]}"#, "expected string")]
#[case::authorities_bad_delimiter(
    r#"{"name":"a","authorities":["x"}"#,
    "expected ',' or ']' in principal authorities"
)]
#[case::attributes_not_object(r#"{"name":"a","attributes":[]}"#, "expected '{'")]
#[case::attribute_value_not_string(r#"{"name":"a","attributes":{"k":1}}"#, "expected string")]
#[case::attribute_missing_colon(r#"{"name":"a","attributes":{"k" "v"}}"#, "expected ':'")]
#[case::attributes_bad_delimiter(
    r#"{"name":"a","attributes":{"k":"v"]}"#,
    "expected ',' or '}' in principal attributes"
)]
#[case::object_bad_delimiter(r#"{"name":"a"]"#, "expected ',' or '}' in principal object")]
#[case::bad_unknown_value(r#"{"name":"a","x":tru}"#, "invalid literal")]
#[case::key_not_string(r#"{name:"a"}"#, "expected string")]
#[case::key_missing_colon(r#"{"name" "a"}"#, "expected ':'")]
fn hand_and_serde_reject_identically(#[case] principal: &str, #[case] expected: &str) {
    let input = header(principal);
    let error = parse_wire_header(&input).expect_err("hand rejects");
    assert!(
        error.ends_with(expected),
        "expected {expected:?}, got {error:?}"
    );
    assert!(parse_wire_header_serde(&input).is_err(), "serde rejects");
}

#[test]
fn duplicate_principal_field_is_rejected() {
    let input = br#"{"v":1,"method":"GET","path":"/p","principal":null,"principal":null}"#;
    let error = parse_wire_header(input).expect_err("duplicate rejected");
    assert!(error.ends_with("duplicate field `principal`"), "{error}");
    assert!(parse_wire_header_serde(input).is_err());
}

#[rstest::rstest]
#[case::authorities("authorities", |i: usize| format!("\"r{i}\""), '[', ']')]
#[case::attributes("attributes", |i: usize| format!("\"k{i}\":\"v\""), '{', '}')]
fn entry_cap_is_enforced(
    #[case] field: &str,
    #[case] entry: fn(usize) -> String,
    #[case] open: char,
    #[case] close: char,
) {
    let list = |n: usize| (0..n).map(entry).collect::<Vec<_>>().join(",");
    let at_cap = header(&format!(
        r#"{{"name":"a","{field}":{open}{}{close}}}"#,
        list(MAX_PRINCIPAL_ENTRIES)
    ));
    assert!(
        parse_wire_header(&at_cap).is_ok(),
        "exactly the cap is accepted"
    );

    let over = header(&format!(
        r#"{{"name":"a","{field}":{open}{}{close}}}"#,
        list(MAX_PRINCIPAL_ENTRIES + 1)
    ));
    let error = parse_wire_header(&over).expect_err("over the cap");
    assert!(
        error.ends_with(&format!("too many principal {field} (more than 1024)")),
        "{error}"
    );
}

#[test]
fn plain_strings_borrow_and_convert_to_an_owned_principal() {
    let input = header(r#"{"name":"alice","authorities":["R"],"attributes":{"k":"1","k":"2"}}"#);
    let parsed = parse_wire_header(&input).expect("valid");
    let wire = parsed.principal.as_ref().expect("present");
    assert!(matches!(wire.name, Cow::Borrowed("alice")));
    assert!(matches!(wire.authorities[0], Cow::Borrowed("R")));

    let principal = wire.to_principal();
    assert_eq!(
        principal,
        crate::Principal {
            name: "alice".to_owned(),
            authorities: vec!["R".to_owned()],
            attributes: BTreeMap::from([("k".to_owned(), "2".to_owned())]),
        }
    );
    assert_eq!(parsed.connection_info().principal, Some(principal));
}
