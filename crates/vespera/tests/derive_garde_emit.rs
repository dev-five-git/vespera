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

// ── nested validation via `#[schema(dive)]` ──────────────────────────

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct Address {
    #[schema(min_length = 1, max_length = 64)]
    pub city: String,
    #[schema(pattern = "^[A-Z0-9-]+$")]
    pub postal_code: String,
}

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct LineItem {
    #[schema(min_length = 1)]
    pub sku: String,
    #[schema(minimum = 1)]
    pub quantity: u32,
}

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code, clippy::struct_field_names)]
struct Order {
    #[schema(min_length = 1)]
    pub order_id: String,

    #[schema(dive)]
    pub primary_address: Address,

    #[schema(dive)]
    pub billing_address: Option<Address>,

    #[schema(min_items = 1, max_items = 100, dive)]
    pub line_items: Vec<LineItem>,
}

fn good_order() -> Order {
    Order {
        order_id: "ORD-001".to_owned(),
        primary_address: Address {
            city: "Seoul".to_owned(),
            postal_code: "12345".to_owned(),
        },
        billing_address: None,
        line_items: vec![LineItem {
            sku: "SKU-1".to_owned(),
            quantity: 2,
        }],
    }
}

#[test]
fn nested_validation_clean_order_passes() {
    assert!(good_order().validate().is_ok());
}

#[test]
fn nested_validation_inner_field_violation_reports_dotted_path() {
    let mut o = good_order();
    o.primary_address.city = String::new(); // violates min_length = 1
    let report = o.validate().expect_err("nested validation must fail");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(
        paths.iter().any(|p| p == "primary_address.city"),
        "expected dotted path `primary_address.city`, got {paths:?}"
    );
}

#[test]
fn nested_validation_option_none_skips_inner_checks() {
    // billing_address = None → inner validation must not run, no
    // billing_address.* errors in the report.
    let o = good_order();
    assert!(o.billing_address.is_none());
    assert!(o.validate().is_ok());
}

#[test]
fn nested_validation_option_some_runs_inner_checks() {
    let mut o = good_order();
    o.billing_address = Some(Address {
        city: String::new(),             // violates min_length = 1
        postal_code: "ZZ999".to_owned(), // valid pattern
    });
    let report = o
        .validate()
        .expect_err("billing_address Some must validate");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(
        paths.iter().any(|p| p == "billing_address.city"),
        "expected `billing_address.city`, got {paths:?}"
    );
}

#[test]
fn nested_validation_vec_iterates_with_indexed_path() {
    let mut o = good_order();
    o.line_items = vec![
        LineItem {
            sku: "OK-1".to_owned(),
            quantity: 1,
        },
        LineItem {
            sku: String::new(), // violates min_length=1 at index 1
            quantity: 0,        // violates minimum=1 at index 1
        },
    ];
    let report = o.validate().expect_err("line_items[1] should fail");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(
        paths.iter().any(|p| p == "line_items[1].sku"),
        "expected indexed path `line_items[1].sku`, got {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "line_items[1].quantity"),
        "expected indexed path `line_items[1].quantity`, got {paths:?}"
    );
}

#[test]
fn nested_validation_vec_min_items_and_dive_both_enforced() {
    let mut o = good_order();
    o.line_items.clear(); // violates min_items = 1
    let report = o.validate().expect_err("empty line_items must fail");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(
        paths.iter().any(|p| p == "line_items"),
        "expected outer `line_items` length error, got {paths:?}"
    );
}

#[test]
fn nested_validation_outer_and_inner_violations_both_reported() {
    let mut o = good_order();
    o.order_id = String::new(); // outer min_length=1
    o.primary_address.postal_code = "lowercase".to_owned(); // inner pattern
    let report = o.validate().expect_err("two-level failure");
    let paths: Vec<String> = report.iter().map(|(p, _)| p.to_string()).collect();
    assert!(paths.iter().any(|p| p == "order_id"));
    assert!(paths.iter().any(|p| p == "primary_address.postal_code"));
}
