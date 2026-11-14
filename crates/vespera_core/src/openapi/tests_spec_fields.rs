//! OpenAPI 3.1 fields and `x-*` extensions survive a deserialize →
//! serialize round trip instead of being silently dropped.

use super::*;
use crate::Extensions;
use serde_json::json;

fn full_document() -> serde_json::Value {
    json!({
        "openapi": "3.1.0",
        "x-tagGroups": [{"name": "Core", "tags": ["users"]}],
        "info": {"title": "API", "version": "1.0.0", "x-logo": {"url": "logo.png"}},
        "paths": {
            "/users/{id}": {
                "x-path-owner": "team-a",
                "parameters": [{
                    "name": "id",
                    "in": "path",
                    "required": true,
                    "deprecated": true,
                    "content": {"application/json": {"schema": {"type": "string"}}},
                }],
                "get": {
                    "x-rate-limit": 10,
                    "parameters": [{
                        "name": "q",
                        "in": "query",
                        "allowEmptyValue": true,
                        "allowReserved": true,
                        "examples": {"one": {"externalValue": "https://example.com/q.txt"}},
                    }],
                    "responses": {
                        "200": {
                            "description": "ok",
                            "headers": {
                                "X-Rate": {
                                    "description": "rate",
                                    "required": true,
                                    "deprecated": false,
                                    "schema": {"type": "integer"},
                                    "style": "simple",
                                    "explode": false,
                                    "example": 5,
                                },
                                "X-Rich": {
                                    "examples": {"a": {"value": "x"}},
                                    "content": {"text/plain": {"schema": {"type": "string"}}},
                                },
                            },
                            "links": {"self": {"operationId": "getUser"}},
                        }
                    },
                },
            }
        },
        "components": {
            "links": {"next": {"$ref": "#/components/links/other"}},
            "callbacks": {"hook": {"{$request.body#/url}": {"post": {"responses": {}}}}},
            "pathItems": {"shared": {"get": {"responses": {}}}},
        },
    })
}

#[test]
fn openapi_31_fields_and_extensions_round_trip() {
    let expected = full_document();
    let doc: OpenApi = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(serde_json::to_value(&doc).unwrap(), expected);
}

#[test]
fn unknown_non_extension_keys_are_still_ignored() {
    let doc: OpenApi = serde_json::from_value(json!({
        "openapi": "3.1.0",
        "info": {"title": "API", "version": "1", "unknownField": 1, "x-ok": true},
        "paths": {},
        "jsonSchemaDialect": "https://spec.openapis.org/oas/3.1/dialect/base",
    }))
    .unwrap();
    assert!(doc.extensions.is_empty());
    assert_eq!(
        doc.info.extensions,
        Extensions::from([("x-ok".to_owned(), json!(true))])
    );
}

#[test]
fn only_extension_keys_are_serialized() {
    let mut doc = OpenApi::default();
    doc.extensions.insert("paths".to_owned(), json!("shadow"));
    doc.extensions.insert("x-kept".to_owned(), json!(1));
    let value = serde_json::to_value(&doc).unwrap();
    assert_eq!(value["paths"], json!({}));
    assert_eq!(value["x-kept"], json!(1));
}

#[test]
fn merge_unions_top_level_extensions_self_wins() {
    let mut base = OpenApi::default();
    base.extensions.insert("x-shared".to_owned(), json!("base"));
    let mut other = OpenApi::default();
    other
        .extensions
        .insert("x-shared".to_owned(), json!("other"));
    other.extensions.insert("x-new".to_owned(), json!("other"));

    base.merge(other);

    assert_eq!(
        base.extensions,
        Extensions::from([
            ("x-new".to_owned(), json!("other")),
            ("x-shared".to_owned(), json!("base")),
        ])
    );
}
