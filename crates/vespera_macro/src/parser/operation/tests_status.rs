//! Declared statuses vs inferred responses: only declared ERROR responses
//! (`>= 400`) replace the inferred `400`, `status = N` never overwrites an
//! explicit `responses = [(N, T)]`, and every no-body success status (`204`,
//! `205`) drops the body.

use std::collections::HashMap;

use rstest::rstest;

use super::*;

/// The responses of `sig` under the given declarations, as `status: media
/// types (schema)` lines.
fn render(
    sig: &str,
    success_status: Option<u16>,
    typed: &[(u16, &str)],
    error_status: &[u16],
) -> String {
    let sig: syn::Signature = syn::parse_str(sig).unwrap();
    let known = HashSet::from(["User", "Created"]);
    let definitions = HashMap::from([
        ("User", "pub struct User { pub id: u32 }"),
        ("Created", "pub struct Created { pub id: u32 }"),
    ]);
    let typed: Vec<(u16, String)> = typed
        .iter()
        .map(|(code, name)| (*code, (*name).to_string()))
        .collect();
    let op = build_operation_from_function(
        &sig,
        "/x",
        &known,
        &definitions,
        OperationRouteConfig {
            success_status,
            typed_responses: (!typed.is_empty()).then_some(typed.as_slice()),
            error_status: (!error_status.is_empty()).then_some(error_status),
            ..OperationRouteConfig::default()
        },
    );
    op.responses
        .iter()
        .map(|(code, response)| {
            format!(
                "{code}: {}",
                serde_json::to_string(&response.content).unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[rstest]
// A 2xx entry in `responses = [...]` declares no error: the inferred `400`
// stays.
#[case::typed_2xx_keeps_400(
    "typed_2xx_keeps_400",
    "fn f() -> Result<Json<User>, String>",
    None,
    &[(202, "Created")][..],
    &[][..]
)]
#[case::typed_404_drops_400(
    "typed_404_drops_400",
    "fn f() -> Result<Json<User>, String>",
    None,
    &[(404, "User")][..],
    &[][..]
)]
// `status = 201` re-keys the inferred body unless `responses` already
// documents `201` explicitly.
#[case::status_keeps_explicit_entry(
    "status_keeps_explicit_entry",
    "fn f() -> Json<User>",
    Some(201),
    &[(201, "Created")][..],
    &[][..]
)]
#[case::status_205_drops_body(
    "status_205_drops_body",
    "fn f() -> Json<User>",
    Some(205),
    &[][..],
    &[][..]
)]
// Round 11: an explicit `(200, T)` is another response: `status = 201`
// moves only the inferred success, and an `error_status` entry never
// replaces it.
#[case::status_keeps_explicit_200(
    "status_keeps_explicit_200",
    "fn f() -> Json<Created>",
    Some(201),
    &[(200, "User")][..],
    &[][..]
)]
#[case::status_beats_error_status(
    "status_beats_error_status",
    "fn f() -> Result<Json<Created>, Json<User>>",
    Some(201),
    &[][..],
    &[201][..]
)]
// Round 12: an `error_status` that cannot carry content (1xx, 204, 205,
// 304) is documented without the error body.
#[case::error_status_no_body(
    "error_status_no_body",
    "fn f() -> Result<Json<Created>, Json<User>>",
    None,
    &[][..],
    &[101, 204, 205, 304, 404][..]
)]
// Round 14: so is a typed `responses = [(204, T)]` entry.
#[case::typed_no_body(
    "typed_no_body",
    "fn f() -> Json<Created>",
    None,
    &[(101, "User"), (204, "User"), (205, "User"), (304, "User"), (202, "User")][..],
    &[][..]
)]
fn declared_statuses(
    #[case] name: &str,
    #[case] sig: &str,
    #[case] success_status: Option<u16>,
    #[case] typed: &[(u16, &str)],
    #[case] error_status: &[u16],
) {
    insta::assert_snapshot!(
        format!("declared_statuses_{name}"),
        render(sig, success_status, typed, error_status)
    );
}

/// Declared 1xx / 3xx statuses are not labelled "Error response".
#[rstest]
#[case::informational(&[(101, "User")][..], &[][..], "101", "Informational response")]
#[case::typed_redirect(&[(303, "User")][..], &[][..], "303", "Redirect")]
#[case::error_status_redirect(&[][..], &[304][..], "304", "Redirect")]
#[case::error(&[][..], &[404][..], "404", "Error response")]
fn declared_status_descriptions(
    #[case] typed: &[(u16, &str)],
    #[case] error_status: &[u16],
    #[case] code: &str,
    #[case] description: &str,
) {
    let sig: syn::Signature = syn::parse_str("fn f() -> Json<User>").unwrap();
    let known = HashSet::from(["User"]);
    let definitions = HashMap::from([("User", "pub struct User { pub id: u32 }")]);
    let typed: Vec<(u16, String)> = typed
        .iter()
        .map(|(code, name)| (*code, (*name).to_string()))
        .collect();
    let op = build_operation_from_function(
        &sig,
        "/x",
        &known,
        &definitions,
        OperationRouteConfig {
            typed_responses: (!typed.is_empty()).then_some(typed.as_slice()),
            error_status: (!error_status.is_empty()).then_some(error_status),
            ..OperationRouteConfig::default()
        },
    );
    assert_eq!(op.responses[code].description, description);
}
