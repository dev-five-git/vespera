//! A generation that fails leaves no thread-local state behind: its installed
//! stored defaults and request-view names are cleared on the error exit too
//! (`GenerationState`), so the next `schema!` / `schema_type!` expansion on
//! the same thread never reads a failed generation's maps.

use serde_json::json;

use super::defaults::{snapshot_stored_defaults, stored_defaults_of};
use super::tests_accuracy::{post, renamed, schema, stored_post};
use crate::metadata::CollectedMetadata;
use crate::openapi_generator::try_generate_openapi_doc_with_metadata;

#[test]
fn failed_generation_clears_the_stored_defaults_it_installed() {
    let mut defaulted = schema("Config", "struct Config { limit: u32 }");
    defaulted
        .field_defaults
        .insert("limit".to_string(), json!(3));
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![
        defaulted,
        // Two renames of one Rust ident, used by a route signature: the
        // ambiguity error is raised after the defaults were installed.
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
    ];
    metadata.routes.push(post("/h", "h"));
    let storage = vec![stored_post(
        "h",
        "async fn h(Json(body): Json<Foo>) -> String",
    )];

    // The premise: the defaults this generation installs are not empty.
    assert!(!stored_defaults_of(&metadata).is_empty());

    let result =
        try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &storage);

    let err = result.expect_err("the ambiguous alias must fail the generation");
    assert!(
        err.to_string().contains("ambiguous type `Foo`"),
        "failed for an unrelated reason: {err}"
    );
    assert!(
        snapshot_stored_defaults().is_empty(),
        "a failed generation left its stored defaults installed"
    );
}
