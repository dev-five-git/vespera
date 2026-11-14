use std::{collections::BTreeMap, fs, path::PathBuf};

use rstest::rstest;
use serde_json::{Value, json};
use tempfile::TempDir;
use vespera_core::schema::SchemaRef;

use super::*;
use crate::{
    metadata::{CollectedMetadata, RouteMetadata, StructMetadata},
    openapi_generator::{
        generate_openapi_doc_with_metadata, try_generate_openapi_doc_with_metadata,
    },
};

fn create_temp_file(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
    let file_path = dir.path().join(filename);
    fs::write(&file_path, content).expect("Failed to write temp file");
    file_path
}

fn route_meta(path: &str, fn_name: &str, file_path: &str) -> RouteMetadata {
    RouteMetadata {
        method: "GET".to_string(),
        path: path.to_string(),
        function_name: fn_name.to_string(),
        module_path: format!("test::{fn_name}"),
        file_path: file_path.to_string(),
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

fn struct_meta(name: &str, definition: &str) -> StructMetadata {
    StructMetadata {
        name: name.to_string(),
        definition: definition.to_string(),
        ..Default::default()
    }
}

fn schemas(
    doc: &vespera_core::openapi::OpenApi,
) -> &BTreeMap<String, vespera_core::schema::Schema> {
    doc.components
        .as_ref()
        .and_then(|c| c.schemas.as_ref())
        .expect("schemas present")
}

fn property_default<'a>(
    schema: &'a vespera_core::schema::Schema,
    field_name: &str,
) -> Option<&'a Value> {
    let SchemaRef::Inline(prop_schema) = schema.properties.as_ref()?.get(field_name)? else {
        return None;
    };
    prop_schema.default.as_ref()
}

#[test]
fn schema_lookups_include_hidden_structs_for_references() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(StructMetadata {
        name: "Hidden".to_string(),
        definition: "struct Hidden { id: i32 }".to_string(),
        include_in_openapi: false,
        explicit_schema_name: false,
        field_defaults: BTreeMap::new(),
        source_identity: None,
        rust_ident: None,
    });

    let (known_schema_names, struct_definitions) = build_schema_lookups(&metadata);

    assert!(known_schema_names.contains("Hidden"));
    assert_eq!(
        *struct_definitions.get("Hidden").unwrap(),
        "struct Hidden { id: i32 }"
    );
}

/// An internally tagged newtype variant over a `$ref` to a component that
/// is known to be a non-object (`struct UserId(String)` → `string`) is a
/// compile error; an object component or an unknown `$ref` is accepted.
#[rstest]
#[case::string_newtype_component("string", "struct Inner(String);", true)]
#[case::object_component("object", "struct Inner { id: i32 }", false)]
#[case::unknown_component("unknown", "struct Unrelated { id: i32 }", false)]
fn internally_tagged_newtype_over_non_object_ref(
    #[case] name: &str,
    #[case] inner_definition: &str,
    #[case] is_error: bool,
) {
    let mut metadata = CollectedMetadata::new();
    let inner_name = if name == "unknown" {
        "Unrelated"
    } else {
        "Inner"
    };
    metadata
        .structs
        .push(struct_meta(inner_name, inner_definition));
    metadata.structs.push(struct_meta(
        "Event",
        r#"#[serde(tag = "type")] enum Event { Ping, Data(Inner) }"#,
    ));
    let result =
        try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    assert_eq!(result.is_err(), is_error, "{name}");
    if let Err(error) = result {
        insta::assert_snapshot!(
            format!("internally_tagged_newtype_ref_error_{name}"),
            error.to_string()
        );
    }
}

/// The post-build check only fires on the exact
/// `allOf: [{tag object}, {$ref: "#/components/schemas/<non-object>"}]`
/// shape; any other discriminated `oneOf` branch is left alone, and a tag
/// schema it cannot read is reported as variant `?` rather than skipped.
#[rstest]
#[case::ref_branch(json!({ "$ref": "#/components/schemas/Inner" }), None)]
#[case::external_ref(
    json!({ "allOf": [
        { "type": "object", "properties": { "type": { "type": "string", "enum": ["Data"] } } },
        { "$ref": "other.json#/Inner" }
    ] }),
    None
)]
#[case::ref_tag_schema(
    json!({ "allOf": [
        { "type": "object", "properties": { "type": { "$ref": "#/components/schemas/Tag" } } },
        { "$ref": "#/components/schemas/Inner" }
    ] }),
    Some("`?`")
)]
fn internally_tagged_check_inspects_only_tag_object_plus_local_ref(
    #[case] branch: Value,
    #[case] error_variant: Option<&str>,
) {
    let schemas: BTreeMap<String, vespera_core::schema::Schema> = serde_json::from_value(json!({
        "Inner": { "type": "string" },
        "Event": {
            "discriminator": { "propertyName": "type" },
            "oneOf": [branch]
        }
    }))
    .expect("hand-built components deserialize");
    match (
        reject_internally_tagged_non_object_refs(&schemas),
        error_variant,
    ) {
        (Ok(()), None) => {}
        (Err(error), Some(variant)) => {
            let message = error.to_string();
            assert!(message.starts_with("`Event`: "), "{message}");
            assert!(message.contains(variant), "{message}");
        }
        (got, want) => panic!("got {got:?}, want error variant {want:?}"),
    }
}

#[rstest]
#[case::struct_schema("User", "struct User { id: i32, name: String }")]
#[case::enum_schema("Status", "enum Status { Active, Inactive, Pending }")]
#[case::enum_with_data(
    "Message",
    "enum Message { Text(String), User { id: i32, name: String } }"
)]
fn valid_component_definitions_are_included(#[case] name: &str, #[case] definition: &str) {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(struct_meta(name, definition));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    assert!(schemas(&doc).contains_key(name));
}

#[test]
fn non_struct_non_enum_component_definitions_are_skipped() {
    let mut metadata = CollectedMetadata::new();
    metadata
        .structs
        .push(struct_meta("Config", "const CONFIG: i32 = 42;"));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    assert!(doc.components.is_none() || doc.components.as_ref().unwrap().schemas.is_none());
}

#[test]
fn unparseable_component_definition_surfaces_error() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(StructMetadata {
        name: "Invalid".to_string(),
        definition: "struct { invalid syntax {{{{".to_string(),
        include_in_openapi: true,
        explicit_schema_name: false,
        field_defaults: BTreeMap::new(),
        source_identity: None,
        rust_ident: None,
    });

    let err = try_generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[])
        .expect_err("invalid component metadata must surface as an error");

    assert!(
        err.to_string()
            .contains("failed to parse component schema `Invalid` metadata"),
        "unexpected error: {err}"
    );
}

#[test]
fn enum_schema_and_route_are_generated_together() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let route_file = create_temp_file(
        &temp_dir,
        "status_route.rs",
        "pub fn get_status() -> Status { Status::Active }",
    );

    let mut metadata = CollectedMetadata::new();
    metadata
        .structs
        .push(struct_meta("Status", "enum Status { Active, Inactive }"));
    metadata.routes.push(route_meta(
        "/status",
        "get_status",
        &route_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);

    assert!(schemas(&doc).contains_key("Status"));
    assert!(doc.paths.contains_key("/status"));
}

#[test]
fn serde_default_function_sets_property_default() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let route_file = create_temp_file(
        &temp_dir,
        "user.rs",
        r#"
fn default_name() -> String { "John".to_string() }

struct User {
#[serde(default = "default_name")]
name: String,
}

pub fn get_user() -> User { User { name: "Alice".to_string() } }
"#,
    );

    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(struct_meta(
        "User",
        r#"struct User { #[serde(default = "default_name")] name: String }"#,
    ));
    metadata.routes.push(route_meta(
        "/user",
        "get_user",
        &route_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    let user_schema = schemas(&doc).get("User").expect("User schema");

    assert_eq!(property_default(user_schema, "name"), Some(&json!("John")));
}

#[test]
fn serde_simple_default_uses_type_defaults() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let route_file = create_temp_file(
        &temp_dir,
        "config.rs",
        r"
struct Config {
#[serde(default)]
enabled: bool,
#[serde(default)]
count: i32,
}

pub fn get_config() -> Config { Config { enabled: true, count: 0 } }
",
    );

    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(struct_meta(
        "Config",
        r"struct Config { #[serde(default)] enabled: bool, #[serde(default)] count: i32 }",
    ));
    metadata.routes.push(route_meta(
        "/config",
        "get_config",
        &route_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    let config_schema = schemas(&doc).get("Config").expect("Config schema");

    assert_eq!(
        property_default(config_schema, "enabled"),
        Some(&json!(false))
    );
    assert_eq!(property_default(config_schema, "count"), Some(&json!(0)));
}

#[test]
fn struct_file_index_finds_struct_in_another_route_file() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let route1_file = create_temp_file(
        &temp_dir,
        "users.rs",
        "pub fn get_users() -> Vec<User> { vec![] }",
    );
    let route2_file = create_temp_file(
        &temp_dir,
        "user.rs",
        r#"
fn default_name() -> String { "Guest".to_string() }

struct User {
#[serde(default = "default_name")]
name: String,
}

pub fn get_user() -> User { User { name: "Alice".to_string() } }
"#,
    );

    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(struct_meta(
        "User",
        r#"struct User { #[serde(default = "default_name")] name: String }"#,
    ));
    metadata.routes.push(route_meta(
        "/users",
        "get_users",
        &route1_file.to_string_lossy(),
    ));
    metadata.routes.push(route_meta(
        "/user",
        "get_user",
        &route2_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    let user_schema = schemas(&doc).get("User").expect("User schema");

    assert_eq!(property_default(user_schema, "name"), Some(&json!("Guest")));
}

#[test]
fn stored_field_defaults_have_highest_priority() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let route_file = create_temp_file(
        &temp_dir,
        "config.rs",
        r"
struct Config { count: i32, name: String }
pub fn get_config() -> Config { Config { count: 0, name: String::new() } }
",
    );

    let mut metadata = CollectedMetadata::new();
    metadata.structs.push(StructMetadata {
        name: "Config".to_string(),
        definition: "struct Config { count: i32, name: String }".to_string(),
        include_in_openapi: true,
        explicit_schema_name: false,
        field_defaults: BTreeMap::from([
            ("count".to_string(), json!(42)),
            ("name".to_string(), json!("default_name")),
        ]),
        source_identity: None,
        rust_ident: None,
    });
    metadata.routes.push(route_meta(
        "/config",
        "get_config",
        &route_file.to_string_lossy(),
    ));

    let doc = generate_openapi_doc_with_metadata(None, None, None, None, &metadata, None, &[]);
    let config_schema = schemas(&doc).get("Config").expect("Config schema");

    assert_eq!(property_default(config_schema, "count"), Some(&json!(42)));
    assert_eq!(
        property_default(config_schema, "name"),
        Some(&json!("default_name"))
    );
}

fn renamed(name: &str, ident: &str, definition: &str) -> StructMetadata {
    StructMetadata {
        rust_ident: Some(ident.to_string()),
        ..StructMetadata::new(name.to_string(), definition.to_string())
    }
}

/// Two renamed types sharing a Rust ident resolve the same way whatever
/// order the storage handed them over in.
#[test]
fn shared_rust_ident_alias_is_order_independent() {
    let a = renamed("A", "Foo", "struct Foo { a: i32 }");
    let b = renamed("B", "Foo", "struct Foo { b: String }");
    let resolve = |structs: Vec<StructMetadata>| {
        let mut metadata = CollectedMetadata::new();
        metadata.structs = structs;
        let (_, definitions) = build_schema_lookups(&metadata);
        definitions.get("Foo").map(|d| (*d).to_string())
    };
    let forward = resolve(vec![a.clone(), b.clone()]);
    assert_eq!(forward, resolve(vec![b, a]));
    assert_eq!(forward.as_deref(), Some("struct Foo { a: i32 }"));
}

#[rstest::rstest]
// A handler signature uses the shared ident → ambiguous.
#[case::used_by_route(
    vec![renamed("A", "Foo", "struct Foo { a: i32 }"), renamed("B", "Foo", "struct Foo { b: i32 }")],
    "async fn h(Json(body): Json<Foo>) -> String",
    Some("ambiguous type `Foo`: `#[schema(name = ...)]` registers it as `A` and `B`")
)]
// Another schema's field uses it → ambiguous.
#[case::used_by_schema(
    vec![
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
        StructMetadata::new("Wrap".to_string(), "struct Wrap { inner: Foo }".to_string()),
    ],
    "async fn h() -> String",
    Some("ambiguous type `Foo`")
)]
// Nothing names `Foo` → harmless.
#[case::unused(
    vec![renamed("A", "Foo", "struct Foo { a: i32 }"), renamed("B", "Foo", "struct Foo { b: i32 }")],
    "async fn h() -> String",
    None
)]
// A real schema named `Foo` wins over both aliases.
#[case::real_schema_wins(
    vec![
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
        StructMetadata::new("Foo".to_string(), "struct Foo { c: i32 }".to_string()),
    ],
    "async fn h(Json(body): Json<Foo>) -> String",
    None
)]
// One rename per ident is never ambiguous.
#[case::single_rename(
    vec![renamed("A", "Foo", "struct Foo { a: i32 }")],
    "async fn h(Json(body): Json<Foo>) -> String",
    None
)]
fn ambiguous_alias_is_rejected_only_when_used(
    #[case] structs: Vec<StructMetadata>,
    #[case] signature: &str,
    #[case] expected: Option<&str>,
) {
    let mut metadata = CollectedMetadata::new();
    metadata.structs = structs;
    let storage = [crate::route_impl::StoredRouteInfo {
        fn_sig_str: signature.to_string(),
        inline_module: None,
        fn_name: "h".to_string(),
        ..Default::default()
    }];
    let result = check_ambiguous_aliases(&metadata, &HashMap::new(), &storage);
    match expected {
        None => assert!(result.is_ok(), "{result:?}"),
        Some(message) => {
            let err = result.expect_err("ambiguous").to_string();
            assert!(err.contains(message), "{err}");
        }
    }
}

/// A route found only through the file cache contributes its signature.
#[test]
fn ambiguous_alias_used_by_a_parsed_route_file() {
    let mut metadata = CollectedMetadata::new();
    metadata.structs = vec![
        renamed("A", "Foo", "struct Foo { a: i32 }"),
        renamed("B", "Foo", "struct Foo { b: i32 }"),
    ];
    metadata.routes.push(route_meta("/h", "h", "h.rs"));
    let file_cache = HashMap::from([(
        "h.rs".to_string(),
        syn::parse_file("pub struct X; pub async fn h(Json(b): Json<Foo>) {}").unwrap(),
    )]);
    assert!(check_ambiguous_aliases(&metadata, &file_cache, &[]).is_err());
}

#[test]
fn struct_file_index_is_path_ordered() {
    let file_cache = HashMap::from([
        ("b.rs".to_string(), syn::parse_file("struct Dup;").unwrap()),
        ("a.rs".to_string(), syn::parse_file("struct Dup;").unwrap()),
    ]);
    assert_eq!(
        build_struct_file_index(&file_cache).get("Dup"),
        Some(&"a.rs")
    );
}

#[test]
fn mentions_ident_ignores_unparsable_source() {
    assert!(!mentions_ident("\"unterminated", "Foo"));
    assert!(mentions_ident("fn f(x: Vec<(Foo, u8)>)", "Foo"));
    assert!(mentions_ident("struct S { a: Option<Foo> }", "Foo"));
    // Attributes and module-qualified paths are not ambiguous uses.
    assert!(!mentions_ident(
        "#[derive(vespera::Foo)] struct S { a: super::m::Foo }",
        "Foo"
    ));
    assert!(!mentions_ident("fn f(x: Foobar)", "Foo"));
}
