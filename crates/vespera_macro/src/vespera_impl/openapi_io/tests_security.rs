//! Operation-level `security` of a merged child (round-40 M5-01). A sidecar of
//! `openapi_io/tests.rs`, split out to keep that file under the 1000-line cap.

use std::fs;

use tempfile::TempDir;

use super::tests::{merge_input, vespera_dir_of};
use super::*;
use crate::env_access::test_env;

/// A merged child's OWN operations carry `security` too (round-40 M5-01): a
/// typo'd scheme name or an undeclared oauth2 scope there is rejected against
/// the merged `securitySchemes`, with or without a document. `Some(snapshot)`
/// names the rejected cases' snapshot; `None` is an accepted requirement.
#[rstest::rstest]
#[case::declared_bearer(serde_json::json!([{ "childAuth": [] }]), None, true)]
#[case::declared_bearer_without_doc(serde_json::json!([{ "childAuth": [] }]), None, false)]
#[case::declared_scope(serde_json::json!([{ "childOauth": ["read:x"] }]), None, true)]
#[case::declared_scope_without_doc(serde_json::json!([{ "childOauth": ["read:x"] }]), None, false)]
#[case::typo_name(
    serde_json::json!([{ "childAuht": [] }]),
    Some("merged_child_operation_typo_name"),
    true
)]
#[case::typo_name_without_doc(
    serde_json::json!([{ "childAuht": [] }]),
    Some("merged_child_operation_typo_name_without_doc"),
    false
)]
#[case::undeclared_scope(
    serde_json::json!([{ "childOauth": ["write:x"] }]),
    Some("merged_child_operation_undeclared_scope"),
    true
)]
#[case::undeclared_scope_without_doc(
    serde_json::json!([{ "childOauth": ["write:x"] }]),
    Some("merged_child_operation_undeclared_scope_without_doc"),
    false
)]
fn merged_child_operation_security_is_validated(
    #[case] requirement: serde_json::Value,
    #[case] snapshot: Option<&'static str>,
    #[case] with_doc: bool,
) {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = vespera_dir_of(temp_dir.path());
    fs::create_dir_all(&target_dir).unwrap();
    let child = serde_json::json!({
        "openapi": "3.1.0",
        "info": { "title": "child", "version": "1.0.0" },
        "paths": { "/child": { "get": { "responses": {}, "security": requirement } } },
        "components": {
            "securitySchemes": {
                "childAuth": { "type": "http", "scheme": "bearer" },
                "childOauth": {
                    "type": "oauth2",
                    "flows": { "implicit": {
                        "authorizationUrl": "https://auth.example/authorize",
                        "scopes": { "read:x": "read" }
                    } }
                }
            }
        }
    });
    fs::write(
        crate::vespera_impl::merge_lookup::child_spec_path(&target_dir, "plugin_a", "PluginA"),
        child.to_string(),
    )
    .unwrap();

    let _env_guard = test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
    let mut processed = merge_input(syn::parse_quote!(plugin_a::PluginA));
    if !with_doc {
        processed.docs_url = None;
    }
    processed.security_schemes = Some(std::collections::BTreeMap::from([(
        "parentAuth".to_string(),
        serde_json::from_value(serde_json::json!({ "type": "http", "scheme": "basic" })).unwrap(),
    )]));
    let result = generate_and_write_openapi(
        &processed,
        &CollectedMetadata::new(),
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    );
    match result {
        Ok(_) => assert!(
            snapshot.is_none(),
            "{requirement} should have been rejected"
        ),
        Err(error) => {
            let Some(snapshot) = snapshot else {
                panic!("{requirement} should be accepted: {error}");
            };
            insta::assert_snapshot!(snapshot, error.to_string());
        }
    }
}

/// Without a document, a merged child that is missing, unreadable or not
/// located contributes no schemes — the app's own still validate.
#[test]
fn security_without_doc_skips_unusable_children() {
    let temp_dir = TempDir::new().unwrap();
    let target_dir = vespera_dir_of(temp_dir.path());
    fs::write(
        crate::vespera_impl::merge_lookup::child_spec_path(&target_dir, "broken", "Broken"),
        "{ not json",
    )
    .unwrap();
    let mut processed = ProcessedVesperaInput {
        merge: vec![
            syn::parse_quote!(broken::Broken),
            syn::parse_quote!(missing::Missing),
        ],
        security_schemes: Some(std::collections::BTreeMap::from([(
            "parentAuth".to_string(),
            serde_json::from_value(serde_json::json!({ "type": "http", "scheme": "basic" }))
                .unwrap(),
        )])),
        ..ProcessedVesperaInput::default()
    };
    let mut metadata = CollectedMetadata::new();
    metadata.routes.push(
        serde_json::from_value(serde_json::json!({
            "method": "get", "path": "/s", "function_name": "s",
            "module_path": "routes::s", "file_path": "virtual/s.rs",
            "security": [{ "parentAuth": [] }]
        }))
        .unwrap(),
    );
    let run = |processed: &ProcessedVesperaInput| {
        generate_and_write_openapi(
            processed,
            &metadata,
            HashMap::new(),
            &[],
            &mut MergeSpecCache::new(),
        )
        .map(|result| result.spec_json)
    };
    {
        let _env_guard = test_env::set_var("CARGO_MANIFEST_DIR", temp_dir.path());
        assert_eq!(run(&processed).unwrap(), None);
    }
    let _env_guard = test_env::remove_var("CARGO_MANIFEST_DIR");
    assert_eq!(run(&processed).unwrap(), None);
    // Without declared schemes nothing is enforced (opt-in).
    processed.security_schemes = None;
    assert_eq!(run(&processed).unwrap(), None);
}
