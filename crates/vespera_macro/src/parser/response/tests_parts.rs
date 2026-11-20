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
// `Json<AppError>` is not description-only: `Json` is the wrapper, so the
// error keeps `application/json` with the unresolved payload's `{}` schema.
#[case::err_json_unknown("err_json_unknown", "-> Result<User, Json<AppError>>")]
// 13: the error side is keyed like the success one (`Redirect` → `3XX`,
// `NoContent` → `204`, labelled by that status: "Redirect" / "Successful
// response"); an `Infallible` / `!` error never happens.
#[case::err_redirect("err_redirect", "-> Result<Html<String>, Redirect>")]
#[case::err_no_content("err_no_content", "-> Result<Json<User>, (HeaderMap, NoContent)>")]
#[case::redirect_both("redirect_both", "-> Result<Redirect, Redirect>")]
#[case::err_infallible("err_infallible", "-> Result<Json<User>, std::convert::Infallible>")]
#[case::err_never("err_never", "-> Result<Json<User>, !>")]
// 14: a nested response tuple is searched for its body too.
#[case::nested_tuple("nested_tuple", "-> (StatusCode, (HeaderMap, Json<User>))")]
#[case::result_nested_tuple(
    "result_nested_tuple",
    "-> Result<(StatusCode, (CookieJar, String)), (StatusCode, (HeaderMap,))>"
)]
// Round 11: a bare `Result` on the body path is axum's `IntoResponse for
// Result` (the `Ok` body or the `Err` response), never serde's
// `{"Ok": ..}` / `{"Err": ..}`; its error is one more error response.
#[case::nested_result(
    "nested_result",
    "-> Result<Result<Json<User>, (StatusCode, Json<User>)>, StatusCode>"
)]
#[case::tuple_result("tuple_result", "-> (StatusCode, Result<Json<User>, String>)")]
#[case::nested_result_no_content(
    "nested_result_no_content",
    "-> Result<Result<NoContent, String>, StatusCode>"
)]
#[case::result_ok_header_map("result_ok_header_map", "-> Result<(HeaderMap, Json<User>), String>")]
#[case::result_parts_only("result_parts_only", "-> (StatusCode, Result<(StatusCode,), String>)")]
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

/// Two error bodies on one status (the default `400`) are alternatives: the
/// status documents `oneOf` of both, and an identical body appears once.
#[rstest]
#[case::distinct_bodies(
    "Result<Result<Json<User>, (StatusCode, Json<UserErr>)>, (StatusCode, Json<AppError>)>",
    true
)]
#[case::identical_bodies(
    "Result<Result<Json<User>, (StatusCode, Json<AppError>)>, (StatusCode, Json<AppError>)>",
    false
)]
fn error_bodies_sharing_a_status_are_merged(#[case] ty: &str, #[case] distinct: bool) {
    let sig: syn::Signature = syn::parse_str(&format!("fn handler() -> {ty}")).unwrap();
    let known: HashSet<&str> = HashSet::from(["User", "UserErr", "AppError"]);
    let defs: HashMap<&str, &str> = HashMap::from([
        ("User", "pub struct User { pub id: i32 }"),
        ("UserErr", "pub struct UserErr { pub code: i32 }"),
        ("AppError", "pub struct AppError { pub msg: String }"),
    ]);
    let responses = parse_return_type(&sig.output, &known, &defs);
    let content = responses["400"].content.as_ref().expect("400 body");
    let schema = serde_json::to_string(&content["application/json"].schema).unwrap();
    assert_eq!(schema.contains("oneOf"), distinct, "{schema}");
    assert_eq!(schema.contains("UserErr"), distinct, "{schema}");
    assert!(schema.contains("AppError"), "{schema}");
}

/// An error body of another media type than the one already documented on
/// its status is added beside it, not merged into it.
#[test]
fn error_bodies_of_different_media_types_are_kept_apart() {
    let sig: syn::Signature = syn::parse_str(
        "fn handler() -> Result<Result<Json<User>, (StatusCode, String)>, (StatusCode, Json<AppError>)>",
    )
    .unwrap();
    let known: HashSet<&str> = HashSet::from(["User", "AppError"]);
    let defs: HashMap<&str, &str> = HashMap::from([
        ("User", "pub struct User { pub id: i32 }"),
        ("AppError", "pub struct AppError { pub msg: String }"),
    ]);
    let responses = parse_return_type(&sig.output, &known, &defs);
    let content = responses["400"].content.as_ref().expect("400 body");
    let media: Vec<&str> = content.keys().map(String::as_str).collect();
    assert_eq!(media, ["application/json", "text/plain"]);
}

#[test]
fn no_return_type_is_an_empty_200() {
    let responses = parse_return_type(&ReturnType::Default, &HashSet::new(), &HashMap::new());
    assert_eq!(
        serde_json::to_string(&responses).unwrap(),
        r#"{"200":{"description":"Successful response"}}"#
    );
}
