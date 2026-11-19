//! The export spec texts: `OPENAPI_SPEC` / the `include_str!` copy is the pure
//! OpenAPI document in struct field order; only the sidecar the parent merge
//! reads carries the catch-all marker.

use super::*;
use crate::metadata::RouteMetadata;

const MARKER: &str = super::super::merge_lookup::CATCH_ALL_KEY;

fn catch_all_route() -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_string(),
        path: "/files/{*rest}".to_string(),
        function_name: "serve".to_string(),
        module_path: "app::files".to_string(),
        file_path: "src/routes/files.rs".to_string(),
        error_status: None,
        typed_responses: None,
        tags: None,
        security: None,
        headers: Vec::new(),
        success_status: None,
        operation_id: None,
        summary: None,
        request_example: None,
        response_example: None,
        deprecated: false,
        description: None,
    }
}

fn export_texts(routes: Vec<RouteMetadata>) -> (String, String) {
    let mut openapi: vespera_core::OpenApi = serde_json::from_str(
        r#"{"openapi":"3.1.0","info":{"title":"app","version":"1.0.0"},"paths":{}}"#,
    )
    .expect("minimal OpenAPI document");
    let metadata = CollectedMetadata {
        routes,
        structs: Vec::new(),
        crons: Vec::new(),
        cfg_routes: Vec::new(),
        test_only_routes: Vec::new(),
    };
    namespace_and_serialize_export_openapi(&mut openapi, &metadata, "/api")
        .expect("export texts serialize")
}

#[test]
fn pure_spec_has_no_catch_all_marker() {
    let (spec, sidecar) = export_texts(vec![catch_all_route()]);
    assert!(!spec.contains(MARKER), "{spec}");
    assert!(sidecar.contains(MARKER), "{sidecar}");
    let value: serde_json::Value = serde_json::from_str(&spec).expect("pure spec is JSON");
    let keys: Vec<&str> = value
        .as_object()
        .expect("pure spec is an object")
        .keys()
        .map(String::as_str)
        .collect();
    // BTreeMap-backed `Value` would sort; the struct order is openapi, info, paths.
    assert!(spec.starts_with(r#"{"openapi":"3.1.0","info":{"#), "{spec}");
    assert!(keys.contains(&"paths"), "{spec}");
}

#[test]
fn sidecar_keeps_struct_key_order_and_appends_the_marker() {
    let (spec, sidecar) = export_texts(vec![catch_all_route()]);
    let suffix = format!(",\"{MARKER}\":{{\"/files/{{rest}}\":\"rest\"}}}}");
    let body = sidecar
        .strip_suffix(suffix.as_str())
        .unwrap_or_else(|| panic!("marker is the last key: {sidecar}"));
    assert_eq!(
        format!("{body}}}"),
        spec,
        "sidecar minus marker equals the pure spec"
    );
    let markers = super::super::merge_lookup::catch_all_markers(&sidecar)
        .expect("sidecar carries the marker");
    assert_eq!(
        markers.get("/files/{rest}").map(String::as_str),
        Some("rest")
    );
}

#[test]
fn sidecar_marker_is_an_empty_object_without_catch_all_routes() {
    let (spec, sidecar) = export_texts(Vec::new());
    assert!(
        sidecar.ends_with(&format!(",\"{MARKER}\":{{}}}}")),
        "{sidecar}"
    );
    assert!(!spec.contains(MARKER), "{spec}");
}
