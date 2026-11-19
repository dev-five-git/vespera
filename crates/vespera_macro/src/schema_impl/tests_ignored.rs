//! The `schema_type!(.., ignore)` pre-registration follows the derive's
//! duplicate-name rule (R28-MR2-1), whichever of the two expands first.

use super::tests::remove_current_crate_schema;
use super::*;

const DERIVED: &str = "struct UserDto { id: i32, name: String }";
const PICKED: &str = "struct UserDto { id: i32 }";
const DERIVED_FILE: &str = "src/r28_mr2_dto.rs::UserDto";

fn metadata(key: &str, definition: &str, identity: Option<&str>) -> StructMetadata {
    let metadata = StructMetadata::new(key.to_string(), definition.to_string());
    match identity {
        Some(identity) => metadata.with_source_identity(identity.to_string()),
        None => metadata,
    }
}

fn stored_definition(key: &str) -> String {
    current_crate_schemas()
        .get(key)
        .expect("a schema stays registered")
        .definition
        .clone()
}

/// The derive expands first; the `ignore` pre-registration of a different
/// definition under the same name is the duplicate-name error, and the derived
/// entry stays.
#[test]
fn derive_first_then_ignored_conflict_is_refused() {
    let key = "__r28_mr2_derive_first__";
    remove_current_crate_schema(key);
    assert!(register_schema(key.to_string(), metadata(key, DERIVED, Some(DERIVED_FILE))).is_ok());

    let span = proc_macro2::Span::call_site();
    let err = register_ignored_schema(metadata(key, PICKED, None), span)
        .expect_err("a different definition under the name must conflict");
    assert!(
        err.to_string().contains("duplicate vespera Schema name"),
        "{err}"
    );
    assert!(
        stored_definition(key).contains("name"),
        "the derived entry stays"
    );
    remove_current_crate_schema(key);
}

/// The `ignore` pre-registration expands first; the derive of a different
/// definition under the same name is refused by the same rule.
#[test]
fn ignored_first_then_derive_conflict_is_refused() {
    let key = "__r28_mr2_ignored_first__";
    remove_current_crate_schema(key);
    let span = proc_macro2::Span::call_site();
    assert!(register_ignored_schema(metadata(key, PICKED, None), span).is_ok());

    assert!(
        register_schema(key.to_string(), metadata(key, DERIVED, Some(DERIVED_FILE))).is_err(),
        "the derive must hit the same duplicate-name rule"
    );
    assert_eq!(stored_definition(key), PICKED, "the picked entry stays");
    remove_current_crate_schema(key);
}

/// An identical definition re-registered by the pre-registration (an
/// incremental rebuild) is the no-op the derive gives too.
#[test]
fn identical_ignored_reregistration_is_a_no_op() {
    let key = "__r28_mr2_identical__";
    remove_current_crate_schema(key);
    let span = proc_macro2::Span::call_site();
    for _ in 0..2 {
        assert!(register_ignored_schema(metadata(key, PICKED, None), span).is_ok());
    }
    assert_eq!(stored_definition(key), PICKED);
    remove_current_crate_schema(key);
}

/// The compile error names the schema and points at the `#[schema(name)]`
/// fix, the same text the derive emits.
#[test]
fn duplicate_name_error_names_the_schema() {
    let err = duplicate_name_error("UserDto", proc_macro2::Span::call_site());
    let text = err.to_string();
    assert!(text.contains("`UserDto`"), "{text}");
    assert!(text.contains("#[schema(name ="), "{text}");
}
