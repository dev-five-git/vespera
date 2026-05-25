//! Shape tests for [`vespera_inprocess::error_wire`].
//!
//! Verifies that the helper used by the JNI bridge for panic / parse
//! fallback produces a self-consistent wire response that decoders can
//! always read.

use serde_json::Value;
use vespera_inprocess::error_wire;

fn decode(resp: &[u8]) -> (Value, Vec<u8>) {
    assert!(resp.len() >= 4, "too short");
    let len_bytes: [u8; 4] = resp[..4].try_into().expect("4 bytes");
    let header_len = u32::from_be_bytes(len_bytes) as usize;
    assert!(4 + header_len <= resp.len(), "header_len overflows");
    let header: Value = serde_json::from_slice(&resp[4..4 + header_len]).expect("header JSON");
    let body = resp[4 + header_len..].to_vec();
    (header, body)
}

#[test]
fn error_wire_preserves_status_and_message() {
    let (header, body) = decode(&error_wire(418, "I'm a teapot"));
    assert_eq!(header["v"].as_u64(), Some(1));
    assert_eq!(header["status"].as_u64(), Some(418));
    assert_eq!(String::from_utf8(body).unwrap(), "I'm a teapot");
}

#[test]
fn error_wire_carries_text_plain_content_type() {
    let (header, _body) = decode(&error_wire(500, "boom"));
    let ct = header["headers"]["content-type"]
        .as_str()
        .expect("content-type header missing");
    assert_eq!(ct, "text/plain; charset=utf-8");
}

#[test]
fn error_wire_status_stable_across_range() {
    for status in [400u16, 401, 422, 500, 502, 599] {
        let (header, body) = decode(&error_wire(status, "msg"));
        assert_eq!(header["status"].as_u64(), Some(u64::from(status)));
        assert_eq!(body, b"msg");
        assert!(
            !header["metadata"]["version"].as_str().unwrap().is_empty(),
            "metadata.version must be populated"
        );
    }
}
