//! Response bodies vs response parts: `IntoResponseParts` types never form a
//! body, `Json<(A, B)>` is one JSON array, error tuples are searched like the
//! success side, axum's raw text / byte bodies and `NoContent` / `Redirect`.

use std::collections::{HashMap, HashSet};

use rstest::rstest;

use super::*;

fn responses_json(return_type: &str) -> String {
    let sig: syn::Signature = syn::parse_str(&format!("fn handler() {return_type}")).unwrap();
    let known_schemas: HashSet<&str> = HashSet::from(["User"]);
    let struct_definitions: HashMap<&str, &str> =
        HashMap::from([("User", "pub struct User { pub id: i32 }")]);
    let responses = parse_return_type(&sig.output, &known_schemas, &struct_definitions);
    serde_json::to_string_pretty(&responses).unwrap()
}

#[rstest]
// 4: response-parts types as the whole success type or the last tuple element.
#[case::bare_cookie_jar("bare_cookie_jar", "-> CookieJar")]
#[case::bare_header_map("bare_header_map", "-> HeaderMap")]
#[case::bare_header_array("bare_header_array", "-> [(HeaderName, &'static str); 2]")]
#[case::append_headers("append_headers", "-> AppendHeaders<[(HeaderName, String); 1]>")]
#[case::status_typed_header("status_typed_header", "-> (StatusCode, TypedHeader<ContentType>)")]
#[case::result_extension("result_extension", "-> Result<Extension<User>, String>")]
#[case::result_signed_jar_tuple(
    "result_signed_jar_tuple",
    "-> Result<(StatusCode, SignedCookieJar), String>"
)]
#[case::private_jar_extensions("private_jar_extensions", "-> (PrivateCookieJar, Extensions)")]
// 5: `Json<(A, B)>` is a JSON array; `Result<Json<()>, E>` is `null` JSON.
#[case::result_json_tuple("result_json_tuple", "-> Result<Json<(User, i32)>, String>")]
#[case::result_json_unit("result_json_unit", "-> Result<Json<()>, String>")]
// 6: the error payload is found like the success one.
#[case::err_header_map_json("err_header_map_json", "-> Result<User, (HeaderMap, Json<User>)>")]
#[case::err_header_array_text(
    "err_header_array_text",
    "-> Result<User, ([(HeaderName, &'static str); 1], String)>"
)]
#[case::err_status_header_map("err_status_header_map", "-> Result<User, (StatusCode, HeaderMap)>")]
#[case::err_unit("err_unit", "-> Result<User, ()>")]
// 11: axum's raw text / byte bodies, `NoContent`, `Redirect`.
#[case::box_str("box_str", "-> Box<str>")]
#[case::cow_str("cow_str", "-> Cow<'static, str>")]
#[case::cow_bytes("cow_bytes", "-> Cow<'static, [u8]>")]
#[case::byte_array("byte_array", "-> [u8; 4]")]
#[case::static_byte_array("static_byte_array", "-> &'static [u8; 4]")]
#[case::no_content("no_content", "-> NoContent")]
#[case::result_no_content("result_no_content", "-> Result<axum::response::NoContent, String>")]
#[case::redirect("redirect", "-> Redirect")]
#[case::result_redirect("result_redirect", "-> Result<Redirect, String>")]
// 12: an unresolved named type inside a `Result` is description-only.
#[case::err_unresolved("err_unresolved", "-> Result<Json<User>, AppError>")]
#[case::ok_unresolved("ok_unresolved", "-> Result<CustomBody, String>")]
#[case::err_status_unresolved("err_status_unresolved", "-> Result<User, (StatusCode, AppError)>")]
#[case::err_json_unknown("err_json_unknown", "-> Result<User, Json<AppError>>")]
fn response_parts_and_bodies(#[case] name: &str, #[case] return_type: &str) {
    insta::assert_snapshot!(
        format!("response_parts_{name}"),
        responses_json(return_type)
    );
}

#[rstest]
#[case::box_string("Box<String>", false)]
#[case::box_str("Box<str>", true)]
#[case::cow_str("Cow<'static, str>", true)]
#[case::cow_string("Cow<'static, String>", false)]
#[case::bare_box("Box", false)]
fn string_like_wrappers(#[case] ty: &str, #[case] expected: bool) {
    let ty: Type = syn::parse_str(ty).unwrap();
    assert_eq!(is_string_like(&ty), expected);
}

#[rstest]
#[case::cow_bytes("Cow<'static, [u8]>", true)]
#[case::boxed_array("Box<[u8; 2]>", true)]
#[case::cow_str("Cow<'static, str>", false)]
#[case::vec_u8("Vec<u8>", true)]
#[case::vec_i8("Vec<i8>", false)]
#[case::array_i32("[i32; 2]", false)]
#[case::ref_array_i32("&'static [i32; 2]", false)]
#[case::ref_tuple("&'static (u8,)", false)]
#[case::bare_cow("Cow", false)]
fn bytes_like_wrappers(#[case] ty: &str, #[case] expected: bool) {
    let ty: Type = syn::parse_str(ty).unwrap();
    assert_eq!(is_bytes_like(&ty), expected);
}

#[test]
fn no_return_type_is_an_empty_200() {
    let responses = parse_return_type(&ReturnType::Default, &HashSet::new(), &HashMap::new());
    assert_eq!(
        serde_json::to_string(&responses).unwrap(),
        r#"{"200":{"description":"Successful response"}}"#
    );
}
