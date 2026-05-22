//! End-to-end consumer-side test: `#[derive(vespera::Schema)]` with
//! `#[schema(...)]` constraints must produce a working
//! `garde::Validate` impl that rejects bad values and accepts good ones.
//!
//! This is the integration counterpart to the unit tests in
//! `vespera_macro::garde_emit::tests` — the unit tests verify the
//! emitted token-stream *shape*, this file verifies it *actually
//! compiles and runs* against the real garde crate at user-build time.

#![cfg(feature = "validation")]

use ::vespera::__validation::garde::Validate;
use ::vespera::Schema;

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct CreateUser {
    #[schema(min_length = 3, max_length = 32, pattern = "^[a-z0-9_]+$")]
    username: String,

    #[schema(format = "email")]
    email: String,

    #[schema(minimum = 0, maximum = 150)]
    age: u32,

    #[schema(min_items = 1, max_items = 5)]
    tags: Vec<String>,

    #[schema(min_length = 8)]
    nickname: Option<String>,
}

fn fixture(overrides: impl FnOnce(&mut CreateUser)) -> CreateUser {
    let mut u = CreateUser {
        username: "alice".to_owned(),
        email: "alice@example.com".to_owned(),
        age: 30,
        tags: vec!["a".to_owned()],
        nickname: None,
    };
    overrides(&mut u);
    u
}

#[test]
fn valid_payload_passes_validation() {
    let u = fixture(|_| {});
    assert!(
        u.validate().is_ok(),
        "fixture should pass: {:?}",
        u.validate().unwrap_err()
    );
}

#[test]
fn min_length_violation_is_reported_with_field_path() {
    let u = fixture(|u| u.username = "ab".to_owned()); // 2 < min_length 3
    let report = u.validate().expect_err("validation should fail");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(
        paths.iter().any(|p| p == "username"),
        "expected `username` in error paths, got {paths:?}"
    );
}

#[test]
fn max_length_violation_is_reported() {
    let u = fixture(|u| u.username = "a".repeat(33));
    let report = u.validate().expect_err("validation should fail");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(paths.iter().any(|p| p == "username"), "got {paths:?}");
}

#[test]
fn pattern_violation_is_reported() {
    // Uppercase chars violate `^[a-z0-9_]+$`.
    let u = fixture(|u| u.username = "Alice".to_owned());
    let report = u.validate().expect_err("validation should fail");
    assert!(report.iter().any(|(p, _)| p.to_string() == "username"));
}

#[test]
fn format_email_violation_is_reported() {
    let u = fixture(|u| u.email = "not-an-email".to_owned());
    let report = u.validate().expect_err("validation should fail");
    assert!(report.iter().any(|(p, _)| p.to_string() == "email"));
}

#[test]
fn range_violation_is_reported_on_numeric_field() {
    let u = fixture(|u| u.age = 999);
    let report = u.validate().expect_err("validation should fail");
    assert!(report.iter().any(|(p, _)| p.to_string() == "age"));
}

#[test]
fn vec_min_items_violation_is_reported() {
    let u = fixture(|u| u.tags.clear());
    let report = u.validate().expect_err("validation should fail");
    assert!(report.iter().any(|(p, _)| p.to_string() == "tags"));
}

#[test]
fn vec_max_items_violation_is_reported() {
    let u = fixture(|u| {
        u.tags = (0..10).map(|i| format!("tag{i}")).collect();
    });
    let report = u.validate().expect_err("validation should fail");
    assert!(report.iter().any(|(p, _)| p.to_string() == "tags"));
}

#[test]
fn option_field_validates_only_when_present() {
    // None — skipped entirely.
    let u = fixture(|u| u.nickname = None);
    assert!(u.validate().is_ok());

    // Some(too short) — fails.
    let u = fixture(|u| u.nickname = Some("hi".to_owned())); // 2 < min_length 8
    let report = u.validate().expect_err("validation should fail");
    assert!(report.iter().any(|(p, _)| p.to_string() == "nickname"));

    // Some(long enough) — passes.
    let u = fixture(|u| u.nickname = Some("longnickname".to_owned()));
    assert!(u.validate().is_ok());
}

#[test]
fn multiple_field_violations_all_reported_in_one_report() {
    let u = fixture(|u| {
        u.username = "X".to_owned(); // pattern + min_length
        u.email = "broken".to_owned(); // format
        u.age = 200; // range
    });
    let report = u.validate().expect_err("validation should fail");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(paths.iter().any(|p| p == "username"));
    assert!(paths.iter().any(|p| p == "email"));
    assert!(paths.iter().any(|p| p == "age"));
    // At least 3 errors collected — exact count may vary because
    // username triggers both pattern and (implicitly satisfied) length.
    assert!(report.iter().count() >= 3, "got {paths:?}");
}
