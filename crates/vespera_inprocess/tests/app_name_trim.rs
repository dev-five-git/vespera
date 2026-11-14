//! App names are trimmed identically everywhere: the runtime registry and
//! the per-request lookup use the same ASCII-only trim as the compile-time
//! `jni_apps!` / `ffi_apps!` checks (`__app_names_valid`), so a name with a
//! non-ASCII space (U+00A0, U+3000) is invalid in all three — never silently
//! accepted at runtime while the macros reject it.

use rstest::rstest;
use serde_json::{Value, json};
use vespera_inprocess::{
    __app_names_valid, Router, dispatch_from_bytes, register_app_named, try_register_app_named,
};

fn request(app: &str) -> Vec<u8> {
    let header =
        serde_json::to_vec(&json!({ "v": 1, "method": "GET", "path": "/", "app": app })).unwrap();
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(&header);
    wire
}

fn status(wire: &[u8]) -> u64 {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    header["status"].as_u64().unwrap()
}

#[rstest]
#[case::ascii_padding(" \t trim_ascii_pad \n", true)]
#[case::nbsp("\u{a0}trim_nbsp\u{a0}", false)]
#[case::ideographic_space("\u{3000}trim_wide\u{3000}", false)]
fn registration_trims_like_the_compile_time_check(#[case] name: &str, #[case] valid: bool) {
    assert_eq!(__app_names_valid(&[name]), valid);
    assert_eq!(try_register_app_named(name, Router::new).is_ok(), valid);
}

#[test]
fn lookup_trims_only_ascii_whitespace() {
    register_app_named("trim_lookup", Router::new);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    // ASCII padding resolves the app (its empty router answers 404 itself,
    // not the "no app registered" 404 — both are 404, so check the body).
    let padded = dispatch_from_bytes(request(" trim_lookup\t"), &rt);
    assert_eq!(status(&padded), 404);
    assert!(
        !String::from_utf8_lossy(&padded).contains("no app registered"),
        "ASCII-padded name must resolve the app"
    );
    // A non-ASCII space is part of the name → invalid → 400.
    let nbsp = dispatch_from_bytes(request("\u{a0}trim_lookup"), &rt);
    assert_eq!(status(&nbsp), 400);
}
