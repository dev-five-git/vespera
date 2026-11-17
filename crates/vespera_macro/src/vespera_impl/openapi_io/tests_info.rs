//! `info` arguments reach the generated document. A sidecar of
//! `openapi_io/tests.rs`, split out to keep that file under the 1000-line cap.

use super::*;
use crate::router_codegen::InfoArgs;

#[test]
fn test_generate_and_write_openapi_applies_info_args() {
    let processed = ProcessedVesperaInput {
        folder_name: "routes".to_string(),
        spec_url: Some("/openapi.json".to_string()),
        info: InfoArgs {
            description: Some("Configured description".to_string()),
            license: Some("MIT".to_string()),
            contact_email: Some("api@example.com".to_string()),
            ..Default::default()
        },
        ..ProcessedVesperaInput::default()
    };
    let result = generate_and_write_openapi(
        &processed,
        &CollectedMetadata::new(),
        HashMap::new(),
        &[],
        &mut MergeSpecCache::new(),
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&result.spec_json.unwrap()).unwrap();
    assert_eq!(doc["info"]["description"], "Configured description");
    assert_eq!(doc["info"]["license"]["identifier"], "MIT");
    assert_eq!(doc["info"]["contact"]["email"], "api@example.com");
}
