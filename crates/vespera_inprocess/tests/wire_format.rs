//! Negative-path tests for the binary wire decoder
//! ([`vespera_inprocess::dispatch_from_bytes`]).
//!
//! NOTE: this test binary does NOT call `register_app`, so the
//! happy-path "unknown app" check naturally produces a 500.  All other
//! checks fail BEFORE the app lookup so the absence of a router is
//! orthogonal.

use serde_json::Value;
use tokio::runtime::Builder;
use vespera_inprocess::dispatch_from_bytes;

fn dispatch(wire: Vec<u8>) -> (Value, Vec<u8>) {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let resp = dispatch_from_bytes(wire, &runtime);
    assert!(resp.len() >= 4, "wire response too short ({})", resp.len());
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(
        4 + header_len <= resp.len(),
        "header_len {header_len} overflows response"
    );
    let header: Value = serde_json::from_slice(&resp[4..4 + header_len])
        .expect("response header is valid JSON");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

#[test]
fn input_shorter_than_4_bytes_returns_400() {
    let (header, body) = dispatch(vec![0x00, 0x01, 0x02]);
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("too short"),
        "expected 'too short' explanation, got {msg}"
    );
}

#[test]
fn empty_input_returns_400() {
    let (header, body) = dispatch(Vec::new());
    assert_eq!(header["status"].as_u64(), Some(400));
    assert!(!body.is_empty(), "error response must include a body");
}

#[test]
fn header_len_exceeding_input_returns_400() {
    // header_len = 99999, but only 4 bytes total
    let header_len: u32 = 99_999;
    let mut wire = Vec::new();
    wire.extend_from_slice(&header_len.to_be_bytes());
    // No JSON, no body — header_len overflows.
    let (header, body) = dispatch(wire);
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("exceeds"),
        "expected 'exceeds' explanation, got {msg}"
    );
}

#[test]
fn header_json_invalid_returns_400() {
    let bad_json = b"this is not json at all";
    let header_len = u32::try_from(bad_json.len()).unwrap();
    let mut wire = Vec::new();
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(bad_json);
    let (header, body) = dispatch(wire);
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("JSON parse"),
        "expected JSON parse error, got {msg}"
    );
}

#[test]
fn wire_version_missing_returns_400_version_mismatch() {
    // No "v" field -> serde default of 0, which != WIRE_VERSION (1).
    let header_json = br#"{"method":"GET","path":"/ping"}"#;
    let header_len = u32::try_from(header_json.len()).unwrap();
    let mut wire = Vec::new();
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(header_json);
    let (header, body) = dispatch(wire);
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("wire version"),
        "expected 'wire version' explanation, got {msg}"
    );
}

#[test]
fn wire_version_wrong_value_returns_400() {
    let header_json = br#"{"v":42,"method":"GET","path":"/ping"}"#;
    let header_len = u32::try_from(header_json.len()).unwrap();
    let mut wire = Vec::new();
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(header_json);
    let (header, body) = dispatch(wire);
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("42"),
        "error must mention the received version, got {msg}"
    );
}

#[test]
fn no_app_registered_returns_404_with_explanatory_body() {
    // Well-formed wire request, but no app has been registered in
    // THIS test binary (we deliberately never call register_app).
    // Multi-app routing treats an unregistered name (including the
    // default "_default") as a 404 — same status HTTP uses for an
    // unknown route — rather than 500.
    let header_json = br#"{"v":1,"method":"GET","path":"/ping"}"#;
    let header_len = u32::try_from(header_json.len()).unwrap();
    let mut wire = Vec::new();
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(header_json);
    let (header, body) = dispatch(wire);
    assert_eq!(header["status"].as_u64(), Some(404));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("no app registered"),
        "expected 'no app registered' explanation, got {msg}"
    );
    assert!(
        msg.contains("_default"),
        "explanation should name the default app, got {msg}"
    );
}

#[test]
fn invalid_app_name_returns_400() {
    // Wire header carries "app" with invalid characters → 400, not 404
    let header_json = br#"{"v":1,"method":"GET","path":"/ping","app":"bad name!"}"#;
    let header_len = u32::try_from(header_json.len()).unwrap();
    let mut wire = Vec::new();
    wire.extend_from_slice(&header_len.to_be_bytes());
    wire.extend_from_slice(header_json);
    let (header, body) = dispatch(wire);
    assert_eq!(header["status"].as_u64(), Some(400));
    let msg = String::from_utf8_lossy(&body);
    assert!(
        msg.contains("invalid app name"),
        "expected 'invalid app name' explanation, got {msg}"
    );
}

#[test]
fn response_content_type_is_text_plain_for_errors() {
    let (header, _body) = dispatch(vec![0u8; 3]); // too short
    assert_eq!(header["status"].as_u64(), Some(400));
    let ct = header["headers"]["content-type"]
        .as_str()
        .expect("error response must have content-type");
    assert!(ct.starts_with("text/plain"), "got {ct}");
}
