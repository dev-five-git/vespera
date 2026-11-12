//! The optional `principal` request wire-header field (set only by the
//! trusted host, never derived from client headers) reaches axum handlers as
//! a `Principal` request extension on every dispatch entry point — buffered,
//! direct-write, response streaming, bidirectional streaming and the text
//! envelope API.  Without it the `Principal` extractor fails closed (`401`).

use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;

use axum::Extension;
use axum::routing::get;
use rstest::rstest;
use vespera_inprocess::{
    DirectWriteResult, MISSING_PRINCIPAL_MSG, Principal, RequestChunk, RequestEnvelope, Router,
    dispatch_bidirectional_streaming_with_header, dispatch_from_bytes_async, dispatch_into_async,
    dispatch_into_async_borrowed, dispatch_owned, dispatch_streaming_with_header_async,
    register_app_named,
};

const APP: &str = "principal_test";

async fn whoami(principal: Principal) -> String {
    let attrs: Vec<String> = principal
        .attributes
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    format!(
        "{}|{}|{}",
        principal.name,
        principal.authorities.join(","),
        attrs.join(",")
    )
}

async fn maybe(principal: Option<Principal>) -> String {
    principal.map_or_else(|| "anonymous".to_owned(), |p| p.name)
}

async fn via_extension(Extension(principal): Extension<Principal>) -> String {
    principal.name
}

fn router() -> Router {
    Router::new()
        .route("/whoami", get(whoami))
        .route("/maybe", get(maybe))
        .route("/ext", get(via_extension))
}

fn register() {
    register_app_named(APP, router);
}

fn wire(path: &str, extra: &str) -> Vec<u8> {
    let header = format!(r#"{{"v":1,"method":"GET","path":"{path}","app":"{APP}"{extra}}}"#);
    let mut out = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out
}

/// `(status, body)` of a complete wire response.
fn status_and_body(wire: &[u8]) -> (u16, String) {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    let status = u16::try_from(header["status"].as_u64().unwrap()).unwrap();
    (
        status,
        String::from_utf8_lossy(&wire[4 + len..]).into_owned(),
    )
}

const ALICE: &str = r#","principal":{"name":"alice","authorities":["ROLE_USER","ROLE_ADMIN"],"attributes":{"tenant":"acme","auth_type":"BASIC"}}"#;
const ALICE_SEEN: &str = "alice|ROLE_USER,ROLE_ADMIN|auth_type=BASIC,tenant=acme";

#[rstest]
#[case::full("/whoami", ALICE, 200, ALICE_SEEN)]
#[case::name_only("/whoami", r#","principal":{"name":"bob"}"#, 200, "bob||")]
#[case::absent_rejects_401("/whoami", "", 401, MISSING_PRINCIPAL_MSG)]
#[case::null_rejects_401("/whoami", r#","principal":null"#, 401, MISSING_PRINCIPAL_MSG)]
#[case::optional_present("/maybe", ALICE, 200, "alice")]
#[case::optional_absent("/maybe", "", 200, "anonymous")]
#[case::extension("/ext", ALICE, 200, "alice")]
#[case::client_header_is_not_a_principal(
    "/whoami",
    r#","headers":{"x-principal":"mallory","principal":"mallory"}"#,
    401,
    MISSING_PRINCIPAL_MSG
)]
#[case::missing_name_is_bad_request("/whoami", r#","principal":{}"#, 400, "")]
#[tokio::test]
async fn buffered_dispatch_inserts_principal(
    #[case] path: &str,
    #[case] extra: &str,
    #[case] status: u16,
    #[case] body: &str,
) {
    register();
    let (got_status, got_body) =
        status_and_body(&dispatch_from_bytes_async(wire(path, extra)).await);
    assert_eq!(got_status, status, "{got_body}");
    if status != 400 {
        assert_eq!(got_body, body);
    }
}

#[tokio::test]
async fn direct_write_dispatch_inserts_principal() {
    register();
    let mut out = vec![0u8; 4096];
    let DirectWriteResult::Complete(n) =
        dispatch_into_async(wire("/whoami", ALICE), &mut out).await
    else {
        panic!("response fits");
    };
    assert_eq!(status_and_body(&out[..n]), (200, ALICE_SEEN.to_owned()));

    let request = wire("/whoami", ALICE);
    let DirectWriteResult::Complete(n) = dispatch_into_async_borrowed(&request, &mut out).await
    else {
        panic!("response fits");
    };
    assert_eq!(status_and_body(&out[..n]), (200, ALICE_SEEN.to_owned()));
}

#[tokio::test]
async fn streaming_dispatch_inserts_principal() {
    register();
    let mut header = Vec::new();
    let mut body = Vec::new();
    let _outcome = dispatch_streaming_with_header_async(
        wire("/whoami", ALICE),
        |h: &[u8]| header.extend_from_slice(h),
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
    )
    .await;
    header.extend_from_slice(&body);
    assert_eq!(status_and_body(&header), (200, ALICE_SEEN.to_owned()));

    let mut header = Vec::new();
    let mut body = Vec::new();
    let _outcome = dispatch_bidirectional_streaming_with_header(
        wire("/whoami", ALICE),
        || RequestChunk::End,
        |chunk: &[u8]| {
            body.extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        |h: &[u8]| header.extend_from_slice(h),
    )
    .await;
    header.extend_from_slice(&body);
    assert_eq!(status_and_body(&header), (200, ALICE_SEEN.to_owned()));
}

#[rstest]
#[case::present(
    Some(Principal {
        name: "carol".to_owned(),
        authorities: vec!["ROLE_X".to_owned()],
        attributes: BTreeMap::from([("k".to_owned(), "v".to_owned())]),
    }),
    200,
    "carol|ROLE_X|k=v"
)]
#[case::absent(None, 401, MISSING_PRINCIPAL_MSG)]
#[tokio::test]
async fn envelope_dispatch_inserts_principal(
    #[case] principal: Option<Principal>,
    #[case] status: u16,
    #[case] body: &str,
) {
    let response = dispatch_owned(
        router(),
        RequestEnvelope {
            method: "GET".into(),
            path: "/whoami".into(),
            query: String::new(),
            headers: HashMap::new(),
            body: String::new(),
            principal,
        },
    )
    .await;
    assert_eq!((response.status, response.body.as_str()), (status, body));
}

#[test]
fn envelope_json_accepts_principal_and_defaults_it() {
    let envelope: RequestEnvelope = serde_json::from_str(
        r#"{"method":"GET","path":"/","principal":{"name":"dave","authorities":["A"]}}"#,
    )
    .unwrap();
    let principal = envelope.principal.unwrap();
    assert_eq!(principal.name, "dave");
    assert_eq!(principal.authorities, ["A"]);
    assert!(principal.attributes.is_empty());

    let bare: RequestEnvelope = serde_json::from_str(r#"{"method":"GET","path":"/"}"#).unwrap();
    assert!(bare.principal.is_none());
}
