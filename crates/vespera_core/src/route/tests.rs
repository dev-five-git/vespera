use super::*;
use rstest::rstest;

#[test]
fn media_type_encoding_serializes_only_when_set() {
    let mut media = MediaType {
        schema: None,
        example: None,
        examples: None,
        encoding: None,
    };
    assert_eq!(serde_json::to_string(&media).unwrap(), "{}");

    media.encoding = Some(BTreeMap::from([(
        "avatar".to_string(),
        Encoding {
            content_type: Some("image/png".to_string()),
            allow_reserved: Some(false),
            ..Encoding::default()
        },
    )]));
    let json = serde_json::to_string(&media).unwrap();
    assert_eq!(
        json,
        r#"{"encoding":{"avatar":{"contentType":"image/png","allowReserved":false}}}"#
    );
    let parsed: MediaType = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed.encoding.unwrap()["avatar"].content_type.as_deref(),
        Some("image/png")
    );
}

fn round_trip<T: Serialize + serde::de::DeserializeOwned>(json: &str) -> String {
    serde_json::to_string(&serde_json::from_str::<T>(json).unwrap()).unwrap()
}

#[rstest]
#[case::parameter_null(round_trip::<Parameter>, r#"{"name":"q","in":"query","example":null}"#)]
#[case::parameter_absent(round_trip::<Parameter>, r#"{"name":"q","in":"query"}"#)]
#[case::media_type_null(round_trip::<MediaType>, r#"{"example":null}"#)]
#[case::media_type_absent(round_trip::<MediaType>, "{}")]
#[case::example_null(round_trip::<Example>, r#"{"value":null}"#)]
#[case::example_absent(round_trip::<Example>, r#"{"summary":"s"}"#)]
#[case::header_null(round_trip::<Header>, r#"{"example":null}"#)]
#[case::header_absent(round_trip::<Header>, "{}")]
#[case::nested_example_null(round_trip::<MediaType>, r#"{"examples":{"none":{"value":null}}}"#)]
fn explicit_null_example_survives_round_trip(
    #[case] round_trip: fn(&str) -> String,
    #[case] json: &str,
) {
    assert_eq!(round_trip(json), json);
}

#[test]
fn path_item_operations_iterates_present_methods_in_order() {
    let mut item = PathItem::default();
    assert_eq!(item.operations().count(), 0);
    for (method, id) in [(HttpMethod::Delete, "d"), (HttpMethod::Get, "g")] {
        item.set_operation(
            method,
            Operation {
                operation_id: Some(id.to_string()),
                ..Operation::default()
            },
        );
    }
    let seen: Vec<(HttpMethod, Option<&str>)> = item
        .operations()
        .map(|(m, op)| (m, op.operation_id.as_deref()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (HttpMethod::Get, Some("g")),
            (HttpMethod::Delete, Some("d"))
        ]
    );
}

#[rstest]
#[case("GET", HttpMethod::Get)]
#[case("get", HttpMethod::Get)]
#[case("Get", HttpMethod::Get)]
#[case("POST", HttpMethod::Post)]
#[case("post", HttpMethod::Post)]
#[case("Post", HttpMethod::Post)]
#[case("PUT", HttpMethod::Put)]
#[case("put", HttpMethod::Put)]
#[case("Put", HttpMethod::Put)]
#[case("PATCH", HttpMethod::Patch)]
#[case("patch", HttpMethod::Patch)]
#[case("Patch", HttpMethod::Patch)]
#[case("DELETE", HttpMethod::Delete)]
#[case("delete", HttpMethod::Delete)]
#[case("Delete", HttpMethod::Delete)]
#[case("HEAD", HttpMethod::Head)]
#[case("head", HttpMethod::Head)]
#[case("Head", HttpMethod::Head)]
#[case("OPTIONS", HttpMethod::Options)]
#[case("options", HttpMethod::Options)]
#[case("Options", HttpMethod::Options)]
#[case("TRACE", HttpMethod::Trace)]
#[case("trace", HttpMethod::Trace)]
#[case("Trace", HttpMethod::Trace)]
fn test_http_method_from_str(#[case] input: &str, #[case] expected: HttpMethod) {
    let result = HttpMethod::try_from(input).unwrap();
    assert_eq!(result, expected);
}

#[test]
fn test_http_method_from_invalid_str() {
    let result = HttpMethod::try_from("INVALID");
    assert!(result.is_err());
}

#[test]
fn test_http_method_serialization() {
    // Test serde serialization (should be UPPERCASE)
    let method = HttpMethod::Get;
    let serialized = serde_json::to_string(&method).unwrap();
    assert_eq!(serialized, "\"GET\"");

    let method = HttpMethod::Post;
    let serialized = serde_json::to_string(&method).unwrap();
    assert_eq!(serialized, "\"POST\"");

    let method = HttpMethod::Delete;
    let serialized = serde_json::to_string(&method).unwrap();
    assert_eq!(serialized, "\"DELETE\"");
}

#[test]
fn test_http_method_deserialization() {
    // Test serde deserialization
    let method: HttpMethod = serde_json::from_str("\"GET\"").unwrap();
    assert_eq!(method, HttpMethod::Get);

    let method: HttpMethod = serde_json::from_str("\"POST\"").unwrap();
    assert_eq!(method, HttpMethod::Post);

    let method: HttpMethod = serde_json::from_str("\"DELETE\"").unwrap();
    assert_eq!(method, HttpMethod::Delete);

    // Case-insensitive like `TryFrom<&str>`; unknown names are rejected.
    let method: HttpMethod = serde_json::from_str("\"get\"").unwrap();
    assert_eq!(method, HttpMethod::Get);
    let method: HttpMethod = serde_json::from_str("\"Trace\"").unwrap();
    assert_eq!(method, HttpMethod::Trace);
    assert!(serde_json::from_str::<HttpMethod>("\"FETCH\"").is_err());
}

#[test]
fn test_path_item_set_operation() {
    let mut path_item = PathItem::default();

    let operation = Operation {
        operation_id: Some("test_operation".to_string()),
        tags: None,
        summary: None,
        description: None,
        parameters: None,
        request_body: None,
        responses: BTreeMap::new(),
        security: None,
        deprecated: None,
        external_docs: None,
        callbacks: None,
        servers: None,
        extensions: crate::Extensions::new(),
    };

    // Test setting GET operation
    path_item.set_operation(HttpMethod::Get, operation.clone());
    assert!(path_item.get.is_some());
    assert_eq!(
        path_item.get.as_ref().unwrap().operation_id,
        Some("test_operation".to_string())
    );

    // Test setting POST operation
    let mut operation_post = operation.clone();
    operation_post.operation_id = Some("post_operation".to_string());
    path_item.set_operation(HttpMethod::Post, operation_post);
    assert!(path_item.post.is_some());
    assert_eq!(
        path_item.post.as_ref().unwrap().operation_id,
        Some("post_operation".to_string())
    );

    // Test setting PUT operation
    let mut operation_put = operation.clone();
    operation_put.operation_id = Some("put_operation".to_string());
    path_item.set_operation(HttpMethod::Put, operation_put);
    assert!(path_item.put.is_some());

    // Test setting PATCH operation
    let mut operation_patch = operation.clone();
    operation_patch.operation_id = Some("patch_operation".to_string());
    path_item.set_operation(HttpMethod::Patch, operation_patch);
    assert!(path_item.patch.is_some());

    // Test setting DELETE operation
    let mut operation_delete = operation.clone();
    operation_delete.operation_id = Some("delete_operation".to_string());
    path_item.set_operation(HttpMethod::Delete, operation_delete);
    assert!(path_item.delete.is_some());

    // Test setting HEAD operation
    let mut operation_head = operation.clone();
    operation_head.operation_id = Some("head_operation".to_string());
    path_item.set_operation(HttpMethod::Head, operation_head);
    assert!(path_item.head.is_some());

    // Test setting OPTIONS operation
    let mut operation_options = operation.clone();
    operation_options.operation_id = Some("options_operation".to_string());
    path_item.set_operation(HttpMethod::Options, operation_options);
    assert!(path_item.options.is_some());

    // Test setting TRACE operation
    let mut operation_trace = operation;
    operation_trace.operation_id = Some("trace_operation".to_string());
    path_item.set_operation(HttpMethod::Trace, operation_trace);
    assert!(path_item.trace.is_some());
}

#[test]
fn test_path_item_set_operation_overwrites() {
    let mut path_item = PathItem::default();

    let operation1 = Operation {
        operation_id: Some("first".to_string()),
        tags: None,
        summary: None,
        description: None,
        parameters: None,
        request_body: None,
        responses: BTreeMap::new(),
        security: None,
        deprecated: None,
        external_docs: None,
        callbacks: None,
        servers: None,
        extensions: crate::Extensions::new(),
    };

    let operation2 = Operation {
        operation_id: Some("second".to_string()),
        tags: None,
        summary: None,
        description: None,
        parameters: None,
        request_body: None,
        responses: BTreeMap::new(),
        security: None,
        deprecated: None,
        external_docs: None,
        callbacks: None,
        servers: None,
        extensions: crate::Extensions::new(),
    };

    // Set first operation
    path_item.set_operation(HttpMethod::Get, operation1);
    assert_eq!(
        path_item.get.as_ref().unwrap().operation_id,
        Some("first".to_string())
    );

    // Overwrite with second operation
    path_item.set_operation(HttpMethod::Get, operation2);
    assert_eq!(
        path_item.get.as_ref().unwrap().operation_id,
        Some("second".to_string())
    );
}

#[test]
fn operation_and_path_item_optional_openapi_fields_serialize_when_present() {
    let mut callbacks = BTreeMap::new();
    callbacks.insert(
        "onEvent".to_string(),
        Callback::from([(
            "{$request.body#/callbackUrl}".to_string(),
            PathItem::default(),
        )]),
    );
    let operation = Operation {
        operation_id: None,
        tags: None,
        summary: None,
        description: None,
        parameters: None,
        request_body: None,
        responses: BTreeMap::new(),
        security: None,
        deprecated: None,
        external_docs: None,
        callbacks: Some(callbacks),
        servers: Some(vec![Server {
            url: "https://api.example.com".to_string(),
            description: None,
            variables: None,
        }]),
        extensions: crate::Extensions::new(),
    };
    let path_item = PathItem {
        ref_path: Some("#/paths/~1users".to_string()),
        get: Some(operation),
        servers: Some(vec![Server {
            url: "https://path.example.com".to_string(),
            description: None,
            variables: None,
        }]),
        ..PathItem::default()
    };

    let json = serde_json::to_string(&path_item).unwrap();

    assert!(json.contains("\"$ref\":\"#/paths/~1users\""));
    assert!(json.contains("callbacks"));
    assert!(json.contains("servers"));
}

/// `callbacks` is name → (runtime expression → Path Item), as in the
/// OpenAPI 3.1 Callback Object; the expression level must survive a
/// round trip instead of being dropped as an unknown Path Item field.
#[test]
fn callbacks_nest_name_then_expression_then_path_item() {
    let json = serde_json::json!({
        "responses": { "200": { "description": "ok" } },
        "callbacks": {
            "onData": {
                "{$request.query.callbackUrl}/data": {
                    "post": { "responses": { "202": { "description": "ok" } } }
                }
            }
        }
    });
    let operation: Operation = serde_json::from_value(json.clone()).unwrap();
    let callback = &operation.callbacks.as_ref().unwrap()["onData"];
    let path_item: &PathItem = &callback["{$request.query.callbackUrl}/data"];
    assert!(path_item.post.is_some());
    assert_eq!(serde_json::to_value(&operation).unwrap(), json);
}

#[rstest]
#[case(HttpMethod::Get, "GET")]
#[case(HttpMethod::Post, "POST")]
#[case(HttpMethod::Put, "PUT")]
#[case(HttpMethod::Patch, "PATCH")]
#[case(HttpMethod::Delete, "DELETE")]
#[case(HttpMethod::Head, "HEAD")]
#[case(HttpMethod::Options, "OPTIONS")]
#[case(HttpMethod::Trace, "TRACE")]
fn test_http_method_display(#[case] method: HttpMethod, #[case] expected: &str) {
    assert_eq!(method.to_string(), expected);
}

#[test]
fn test_http_method_equality() {
    let method1 = HttpMethod::Get;
    let method2 = HttpMethod::Get;
    let method3 = HttpMethod::Post;

    assert_eq!(method1, method2);
    assert_ne!(method1, method3);
}

#[test]
fn test_http_method_clone() {
    let method = HttpMethod::Get;
    let cloned = method;
    assert_eq!(method, cloned);
}

#[test]
fn test_http_method_hash() {
    use std::collections::HashMap;

    let mut map = HashMap::new();
    map.insert(HttpMethod::Get, "GET method");
    map.insert(HttpMethod::Post, "POST method");

    assert_eq!(map.get(&HttpMethod::Get), Some(&"GET method"));
    assert_eq!(map.get(&HttpMethod::Post), Some(&"POST method"));
    assert_eq!(map.get(&HttpMethod::Put), None);
}

/// Round 13: an empty Responses Object is invalid OpenAPI, so an
/// operation without responses omits the key instead of writing `{}`.
#[test]
fn operation_without_responses_omits_the_key() {
    let operation = Operation {
        summary: Some("pet created".to_owned()),
        ..Operation::default()
    };
    assert_eq!(
        serde_json::to_value(&operation).unwrap(),
        serde_json::json!({"summary": "pet created"})
    );
}

/// The documented limitation (module docs, "Reference Objects"): a
/// Reference Object outside a schema position is rejected, not dropped.
#[test]
fn reference_object_outside_a_schema_is_rejected() {
    let error = serde_json::from_value::<Operation>(serde_json::json!({
        "responses": {"200": {"$ref": "#/components/responses/Ok"}}
    }))
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("a Reference Object (`$ref`) is not supported as a Response Object"),
        "{error}"
    );
}

/// The deserialization error text for `value` as `T` (empty when it
/// parses), so one case table can cover every inline-only type.
fn rejection<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> String {
    serde_json::from_value::<T>(value)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default()
}

/// Every inline-only type refuses a `$ref` key, bare or with a
/// `description` sibling that would satisfy its required field. Without
/// the guard a `Response` reference with `description` silently lost the
/// reference, and `Header` / `Example` (all fields optional) became `{}`.
#[rstest]
#[case::header(rejection::<Header>, serde_json::json!({"$ref": "#/components/headers/Rate"}), "Header")]
#[case::example(rejection::<Example>, serde_json::json!({"$ref": "#/components/examples/Ok", "summary": "s"}), "Example")]
#[case::response(rejection::<Response>, serde_json::json!({"$ref": "#/components/responses/Ok"}), "Response")]
#[case::response_with_description(rejection::<Response>, serde_json::json!({"$ref": "#/components/responses/Ok", "description": "Override"}), "Response")]
#[case::request_body(rejection::<RequestBody>, serde_json::json!({"$ref": "#/components/requestBodies/Body", "content": {}}), "RequestBody")]
#[case::parameter(rejection::<Parameter>, serde_json::json!({"$ref": "#/components/parameters/Id"}), "Parameter")]
#[case::parameter_with_fields(rejection::<Parameter>, serde_json::json!({"$ref": "#/components/parameters/Id", "name": "id", "in": "path"}), "Parameter")]
fn reference_object_in_an_inline_only_position_is_rejected(
    #[case] reject: fn(serde_json::Value) -> String,
    #[case] value: serde_json::Value,
    #[case] what: &str,
) {
    let error = reject(value);
    assert!(
        error.contains(&format!(
            "a Reference Object (`$ref`) is not supported as a {what} Object"
        )),
        "{error}"
    );
}

#[test]
fn inline_header_and_example_round_trip() {
    let json = serde_json::json!({
        "description": "ok",
        "headers": {"X-Rate": {"description": "limit", "schema": {"type": "integer"}, "examples": {"one": {"summary": "s", "value": 1}}}},
        "content": {"application/json": {"examples": {"ok": {"externalValue": "https://e.example/ok.json"}}}}
    });
    let response: Response = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&response).unwrap(), json);
    let error =
        serde_json::from_value::<Header>(serde_json::json!({"required": "yes"})).unwrap_err();
    assert!(error.to_string().contains("invalid type"), "{error}");
}
